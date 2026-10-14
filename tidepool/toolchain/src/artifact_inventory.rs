//! Scoped original-artifact ownership. Graph indices are private implementation
//! details; durable and compiler boundaries use content-bound artifact IDs.
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use petgraph::stable_graph::{NodeIndex, StableDiGraph};
use petgraph::visit::EdgeRef;
use petgraph::Direction;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::declaration_join::{ExactInterfaceOwner, ExactModuleIdentity};
use crate::exact_context::{ExactSelectionSnapshot, ImmutableScopeProofRef};
use crate::recovery_artifacts::{CertifiedJoinedInterface, CertifiedRecoveryProduct};
use crate::CompileError;

#[cfg(test)]
mod properties;

mod compiler_projection;
pub(crate) use compiler_projection::CompilerSourceSupport;
pub use compiler_projection::{CompilerInputProjection, CompilerInputRole};
mod scope_proof;
pub use scope_proof::ScopeProofV3;
mod cold_validation;

/// Exact artifacts persist SHA-256 of the compiler's stable producer bytes.
/// Endpoint identities and their raw producer bytes are not this identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CanonicalProducerIdentity([u8; 32]);

impl CanonicalProducerIdentity {
    pub(crate) fn from_producer_bytes(bytes: &[u8]) -> Self {
        Self(Sha256::digest(bytes).into())
    }

    pub(crate) fn from_compiler(identity: &tidepool_extract_cmd::CompilerIdentity) -> Self {
        Self::from_producer_bytes(identity.producer_bytes())
    }

    pub(crate) fn sha256(self) -> [u8; 32] {
        self.0
    }

    pub(crate) fn hex(self) -> String {
        self.0.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    #[cfg(test)]
    pub(crate) fn from_test_sha256(digest: [u8; 32]) -> Self {
        Self(digest)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
pub struct ArtifactId(pub [u8; 32]);

/// Immutable identity of one issued binding scope. Payload identity alone does
/// not identify the canonical children selected by a compiler transaction.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
pub struct ArtifactSelectionId(pub [u8; 32]);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactBinding {
    pub artifact: ArtifactId,
    pub selection: ArtifactSelectionId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeGroupBinding {
    /// Payload and issuing scope of this group node, not an artifact-node
    /// endpoint. Its recorded carrier edge can target an inherited scope.
    pub binding: ArtifactBinding,
    pub original_ordinal: u32,
}

/// Durable facts grant no authority until certified payloads, exact edges and
/// the selected namespace have been validated by the inventory owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactBindingNode {
    Artifact(ArtifactBinding),
    Group(NativeGroupBinding),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactGraphSelection {
    #[serde(
        serialize_with = "serialize_selection_plans",
        deserialize_with = "deserialize_selection_plans"
    )]
    pub selections: Vec<Arc<ScopeProofV3>>,
    pub bindings: Vec<ArtifactBinding>,
    pub namespace: Vec<ArtifactBinding>,
    /// Issued source roots and executable group demand.
    pub roots: Vec<ArtifactBindingNode>,
    pub native_groups: Vec<NativeGroupBinding>,
    /// Exact retained nodes; these seeds grant no executable group demand.
    pub custody_roots: Vec<ArtifactBindingNode>,
}

fn serialize_selection_plans<S: serde::Serializer>(
    plans: &[Arc<ScopeProofV3>],
    serializer: S,
) -> Result<S::Ok, S::Error> {
    use serde::ser::SerializeSeq;
    let mut sequence = serializer.serialize_seq(Some(plans.len()))?;
    for plan in plans {
        sequence.serialize_element(plan.as_ref())?;
    }
    sequence.end()
}

fn deserialize_selection_plans<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<Arc<ScopeProofV3>>, D::Error> {
    Vec::<ScopeProofV3>::deserialize(deserializer)
        .map(|plans| plans.into_iter().map(Arc::new).collect())
}

/// Content-bound original group selection. These serialized facts grant no
/// authority until the inventory checks the certified group and its closure.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeGroupKey {
    pub artifact: ArtifactId,
    pub original_ordinal: u32,
}

/// Immutable issuer-selected artifacts and executable groups. Decoding these
/// facts grants no authority; retention checks them against authenticated live
/// custody and requires their complete exact dependency closure.
#[derive(Clone, Debug)]
pub struct ExactArtifactSelection(Arc<ExactArtifactSelectionFacts>);

/// Observation compared only after the complete selection has been independently
/// reissued from authenticated artifacts. This digest cannot restore authority.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct ExactArtifactSelectionDigest([u8; 32]);

#[derive(Debug)]
struct ExactArtifactSelectionFacts {
    snapshot: Arc<crate::exact_context::ExactSelectionSnapshot>,
    graph: OnceLock<ArtifactGraphSelection>,
    observation: OnceLock<ExactArtifactSelectionDigest>,
}

impl Serialize for ExactArtifactSelection {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut row = serializer.serialize_struct("ExactArtifactSelectionFacts", 1)?;
        row.serialize_field("graph", self.graph_selection())?;
        row.end()
    }
}

impl<'de> Deserialize<'de> for ExactArtifactSelection {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Transport {
            graph: ArtifactGraphSelection,
        }
        Transport::deserialize(deserializer)
            .map(|facts| Self::from_recovered_graph_selection(facts.graph))
    }
}

impl ExactArtifactSelection {
    /// Borrow the issued graph facts without duplicating its complete witnesses.
    /// Consumers still validate these facts against authenticated live custody.
    pub fn graph_selection(&self) -> &ArtifactGraphSelection {
        self.0.graph.get_or_init(|| {
            self.0
                .snapshot
                .graph_selection()
                .expect("issued or checked exact selection shape")
        })
    }

    /// Reconstruct untrusted transport facts, equivalent to deserialization.
    /// This grants no selection authority; the inventory must authenticate the
    /// payloads, digest witnesses and exact closure before using the selection.
    pub fn from_recovered_graph_selection(graph: ArtifactGraphSelection) -> Self {
        let snapshot =
            Arc::new(crate::exact_context::ExactSelectionSnapshot::from_graph_selection(&graph));
        Self(Arc::new(ExactArtifactSelectionFacts {
            snapshot,
            graph: OnceLock::from(graph),
            observation: OnceLock::new(),
        }))
    }

    pub(crate) fn capture(view: &ArtifactView) -> Self {
        Self::from_snapshot(view.selection_snapshot().clone())
    }

    pub(crate) fn from_snapshot(
        snapshot: Arc<crate::exact_context::ExactSelectionSnapshot>,
    ) -> Self {
        Self(Arc::new(ExactArtifactSelectionFacts {
            snapshot,
            graph: OnceLock::new(),
            observation: OnceLock::new(),
        }))
    }

    pub fn snapshot(&self) -> &Arc<crate::exact_context::ExactSelectionSnapshot> {
        &self.0.snapshot
    }

    pub(crate) fn observation_digest(&self) -> Result<ExactArtifactSelectionDigest, CompileError> {
        if let Some(observation) = self.0.observation.get() {
            return Ok(*observation);
        }
        use std::io::Write;

        struct DigestWriter(Sha256);
        impl Write for DigestWriter {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.update(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let mut sink = DigestWriter(Sha256::new());
        sink.0
            .update(b"Tidepool selected binding context observation v4\0");
        {
            let mut buffered = std::io::BufWriter::new(&mut sink);
            let graph = self.graph_selection();
            // Scope identities commit the immutable rows. Proof-only ancestry
            // and its transport layout do not change selected semantics.
            serde_json::to_writer(
                &mut buffered,
                &(
                    &graph.bindings,
                    &graph.namespace,
                    &graph.roots,
                    &graph.native_groups,
                    &graph.custody_roots,
                ),
            )
            .map_err(|error| failure(&format!("exact selection observation: {error}")))?;
            buffered.flush()?;
        }
        let digest = ExactArtifactSelectionDigest(sink.0.finalize().into());
        let _ = self.0.observation.set(digest);
        Ok(digest)
    }
}

impl PartialEq for ExactArtifactSelection {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
            || Arc::ptr_eq(&self.0.snapshot, &other.0.snapshot)
            || self
                .observation_digest()
                .expect("closed exact selection encoding")
                == other
                    .observation_digest()
                    .expect("closed exact selection encoding")
    }
}
impl Eq for ExactArtifactSelection {}

/// Whole-module demand and a checked compiler entry cross the same admission
/// owner. A raw ordinal or recovered selection cannot mint a checked entry.
#[derive(Clone, Copy, Debug)]
pub(crate) enum NativeArtifactDemand<'a> {
    AllGroups,
    /// Checked imports retain original byte custody and interface authority.
    /// They have no executable target that could demand a native group.
    ScopeInterfaces,
    /// A checked authored entry and its same compiled executable wrapper have
    /// distinct native dependencies. Neither implies whole-carrier demand.
    VerifiedTarget {
        entry: &'a crate::checked_cell::CheckedTypedEntry,
        imports: &'a [crate::certified_products::PendingImportOwner],
    },
    /// Existing target certification issued these imports against authenticated
    /// original membership. Only new products receive whole-module demand.
    CertifiedTargetImports(&'a [crate::certified_products::PendingImportOwner]),
}

/// Target imports were issued by the existing target certifier; check exact
/// original membership again before turning source references into graph roots.
fn certified_target_source_groups(
    entries: &[Arc<ArtifactEntry>],
    imports: &[crate::certified_products::PendingImportOwner],
) -> Result<BTreeSet<NativeGroupKey>, CompileError> {
    let mut groups = BTreeSet::new();
    for import in imports {
        let crate::certified_products::PendingImportOwner::Source {
            owner,
            original_ordinal,
            binder,
        } = import
        else {
            continue;
        };
        let mut originals = entries.iter().filter(|entry| {
            matches!(&entry.payload, ArtifactPayload::Original(product)
            if product.owner() == owner
                && crate::certified_products::authenticates_original_native_entry(
                    product, *original_ordinal, binder,
                ))
        });
        let original = originals
            .next()
            .ok_or_else(|| failure("certified target native membership"))?;
        if originals.next().is_some() {
            return Err(failure("certified target native membership"));
        }
        groups.insert(NativeGroupKey {
            artifact: original.descriptor.id,
            original_ordinal: *original_ordinal,
        });
    }
    Ok(groups)
}

/// An original offer carries its issuer's exact selected native rows independently
/// of compiler namespace policy and completed compiler receipts.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct NativeOriginalRequirement {
    pub(crate) original: ArtifactId,
    pub(crate) selection: ExactArtifactSelection,
}

impl NativeOriginalRequirement {
    /// Capture only group handles already selected by this issuer. Byte custody
    /// and an available ordinal census cannot manufacture a group choice.
    pub(crate) fn from_selected_view(
        original: ArtifactId,
        view: &ArtifactView,
    ) -> Result<Option<Self>, CompileError> {
        let state = view.lease.inventory.0.lock().expect("inventory lock");
        let owned = &view.read_projection(&state).nodes;
        if !owned.iter().any(|node| node.artifact() == original)
            || !matches!(
                state.payloads[&original].payload,
                ArtifactPayload::Original(_)
            )
        {
            return Err(failure(
                "native original requirement is outside issuer custody",
            ));
        }
        let roots = view
            .roots()
            .iter()
            .filter(|node| {
                matches!(node,
                InventoryNodeKey::Group(group) if group.binding.artifact == original)
            })
            .copied()
            .collect::<Vec<_>>();
        if roots.is_empty() {
            return Ok(None);
        }
        drop(state);
        let mut selected = view.retain_projected_materializations(view.inventory().retain(
            roots,
            Vec::new(),
            Vec::new(),
            Vec::new(),
        ))?;
        selected.replace_namespace(&BTreeMap::new());
        Ok(Some(Self {
            original,
            selection: ExactArtifactSelection::capture(&selected),
        }))
    }

    /// Qualify private availability from an already issued complete group
    /// closure. Artifact bytes alone cannot supply this proof, and groups from
    /// different original carriers cannot complete each other's census.
    pub(crate) fn from_issued_group_custody(
        original: ArtifactId,
        canonical: ArtifactId,
        view: &ArtifactView,
    ) -> Result<Option<Self>, CompileError> {
        let state = view.lease.inventory.0.lock().expect("inventory lock");
        let projection = view.read_projection(&state);
        let owned = &projection.nodes;
        let Some(canonical_binding) = view.lease.selection.namespace.get(&canonical).copied()
        else {
            return Ok(None);
        };
        let Some(entry) = state.payloads.get(&original) else {
            return Err(failure(
                "private native original is outside retained custody",
            ));
        };
        if !matches!(entry.payload, ArtifactPayload::Original(_)) {
            return Err(admission_failure(
                ArtifactInventoryFailure::NativeRootNotOriginal { artifact: original },
            ));
        }
        let ordinals = &entry.native_group_ordinals;
        if ordinals.is_empty() {
            return Ok(None);
        }
        let index = projection.native_selection(&state);
        let mut complete = Vec::new();
        for node in owned {
            let InventoryNodeKey::Artifact(carrier) = node else {
                continue;
            };
            if carrier.artifact != original
                || view
                    .lease
                    .selection
                    .namespace
                    .get(&original)
                    .is_some_and(|selected| selected != carrier)
                || !state.graph.edges(state.indices[node]).any(|edge| {
                    state.graph[edge.target()] == InventoryNodeKey::Artifact(canonical_binding)
                        && *edge.weight() == ArtifactDependency::Interface
                })
            {
                continue;
            }
            if !ordinals
                .iter()
                .all(|ordinal| index.groups.contains_key(&(*carrier, *ordinal)))
            {
                continue;
            }
            let groups = ordinals
                .iter()
                .map(|ordinal| index.resolve(*carrier, *ordinal))
                .collect::<Result<Vec<_>, _>>()?;
            complete.push((*carrier, groups));
        }
        let Some((carrier, groups)) = complete.pop() else {
            return Ok(None);
        };
        if !complete.is_empty() {
            return Err(admission_failure(
                ArtifactInventoryFailure::BindingAmbiguity { artifact: original },
            ));
        }
        drop(state);
        // Keep the exact group/dependency bindings available, without selecting
        // them for execution. A later target's actual demand owns that choice.
        let mut available = view.retain_projected_materializations(view.inventory().retain(
            vec![InventoryNodeKey::Artifact(carrier)],
            groups.into_iter().map(InventoryNodeKey::Group).collect(),
            Vec::new(),
            Vec::new(),
        ))?;
        available.replace_namespace(&BTreeMap::from([
            (original, carrier),
            (canonical, canonical_binding),
        ]));
        Ok(Some(Self {
            original,
            selection: ExactArtifactSelection::capture(&available),
        }))
    }

    /// Preserve every distinct supplied choice. A published role owns its
    /// explicit selection; ordinary roles use their pre-merge source issuer.
    pub(crate) fn for_projection(
        projection: &CompilerInputProjection,
        view: &ArtifactView,
        preferred: &[Self],
    ) -> Result<Vec<Self>, CompileError> {
        projection.validate(view)?;
        let mut requirements = Vec::new();
        for role in projection.roles() {
            let Some(original) = role.original() else {
                continue;
            };
            let choices = if let Some(issued) = Self::from_role(role) {
                vec![issued]
            } else {
                let selected = preferred
                    .iter()
                    .filter(|fact| fact.original == original)
                    .cloned()
                    .collect::<Vec<_>>();
                if selected.is_empty() {
                    Self::from_selected_view(original, view)?
                        .into_iter()
                        .collect()
                } else {
                    selected
                }
            };
            for choice in choices {
                if !requirements.contains(&choice) {
                    requirements.push(choice);
                }
            }
        }
        Ok(requirements)
    }

    pub(crate) fn from_role(role: CompilerInputRole) -> Option<Self> {
        match role {
            CompilerInputRole::PublishedSourceOriginal {
                original,
                selection,
                ..
            } => Some(Self {
                original,
                selection,
            }),
            _ => None,
        }
    }
}

enum ArtifactRootIntent<'a> {
    SuppliedArtifacts,
    Certified(&'a crate::certified_products::CertifiedSourceSelection),
    Projected(&'a CompilerInputProjection, &'a [NativeOriginalRequirement]),
}

impl ArtifactRootIntent<'_> {
    fn native_original_requirements(&self) -> Vec<NativeOriginalRequirement> {
        match self {
            Self::SuppliedArtifacts => Vec::new(),
            Self::Certified(selection) => {
                selection.native_original_requirements().cloned().collect()
            }
            Self::Projected(projection, preferred) => {
                let mut requirements = projection
                    .roles()
                    .into_iter()
                    .filter_map(NativeOriginalRequirement::from_role)
                    .collect::<Vec<_>>();
                let published = requirements
                    .iter()
                    .map(|fact| fact.original)
                    .collect::<BTreeSet<_>>();
                for requirement in *preferred {
                    if !published.contains(&requirement.original)
                        && !requirements.contains(requirement)
                    {
                        requirements.push(requirement.clone());
                    }
                }
                requirements
            }
        }
    }
}

/// Graph vertices retain one full artifact allocation separately from its
/// admitted native groups. A carrier never implies demand for every group.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
enum LogicalNodeKey {
    Artifact(ArtifactId),
    Group(NativeGroupKey),
}
impl LogicalNodeKey {
    fn artifact(self) -> ArtifactId {
        match self {
            Self::Artifact(id) => id,
            Self::Group(key) => key.artifact,
        }
    }
}

type InventoryNodeKey = ArtifactBindingNode;
impl ArtifactBindingNode {
    pub fn artifact(self) -> ArtifactId {
        match self {
            Self::Artifact(binding) => binding.artifact,
            Self::Group(group) => group.binding.artifact,
        }
    }
    pub fn selection(self) -> ArtifactSelectionId {
        match self {
            Self::Artifact(binding) => binding.selection,
            Self::Group(group) => group.binding.selection,
        }
    }
    /// Typed node membership does not reconstruct a group's artifact carrier.
    pub fn is_retained_in(
        self,
        artifacts: &BTreeSet<ArtifactBinding>,
        groups: &BTreeSet<NativeGroupBinding>,
    ) -> bool {
        match self {
            Self::Artifact(binding) => artifacts.contains(&binding),
            Self::Group(group) => groups.contains(&group),
        }
    }
    fn logical(self) -> LogicalNodeKey {
        match self {
            Self::Artifact(binding) => LogicalNodeKey::Artifact(binding.artifact),
            Self::Group(group) => LogicalNodeKey::Group(NativeGroupKey {
                artifact: group.binding.artifact,
                original_ordinal: group.original_ordinal,
            }),
        }
    }
}
impl LogicalNodeKey {
    fn bind(self, selection: ArtifactSelectionId) -> InventoryNodeKey {
        let binding = ArtifactBinding {
            artifact: self.artifact(),
            selection,
        };
        match self {
            Self::Artifact(_) => InventoryNodeKey::Artifact(binding),
            Self::Group(group) => InventoryNodeKey::Group(NativeGroupBinding {
                binding,
                original_ordinal: group.original_ordinal,
            }),
        }
    }
}

/// Native demand starts at a complete module or one compiler-issued group.
/// Selecting demand does not change the retained artifact custody.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum NativeRequirementRoot {
    AllGroups(ArtifactId),
    /// The ordinal is issued by the original module certificate, independently
    /// of executable entry indices or the position of a cell item.
    Group {
        artifact: ArtifactId,
        original_ordinal: u32,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    OriginalModule,
    CanonicalModuleInterface,
    ValueInterface,
    LexicalJoin,
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactDependency {
    Interface,
    NativeGroup {
        dependent_ordinal: u32,
        required_ordinal: u32,
    },
    NativeBinding {
        dependent_ordinal: u32,
        generation: u64,
        namespace: String,
        occurrence: String,
        record_parent: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ArtifactInventoryFailure {
    #[error("artifact {artifact:?} has differing metadata")]
    MetadataConflict { artifact: ArtifactId },
    #[error("compiler interface owner {owner:?} has differing artifacts")]
    OwnerConflict { owner: ExactModuleIdentity },
    #[error("owner {owner:?} selects multiple native implementations")]
    NativeOwnerAmbiguity { owner: ExactModuleIdentity },
    #[error("artifact {artifact:?} has multiple issued dependency bindings")]
    BindingAmbiguity { artifact: ArtifactId },
    #[error(
        "issued original {artifact:?} group {original_ordinal} differs from its issued selection"
    )]
    NativeGroupSelectionConflict {
        artifact: ArtifactId,
        original_ordinal: u32,
    },
    #[error("compiler owner {owner:?} selects original {existing:?} and {incoming:?}")]
    CompilerOriginalOfferConflict {
        owner: ExactModuleIdentity,
        existing: ArtifactId,
        incoming: ArtifactId,
    },
    #[error("authored generation {generation} requires one certified native root; found {found}")]
    AuthoredNativeRoot { generation: u64, found: usize },
    #[error("native root {artifact:?} is outside its retained view")]
    NativeRootOutsideView { artifact: ArtifactId },
    #[error("native root {artifact:?} has no original native proof")]
    NativeRootNotOriginal { artifact: ArtifactId },
    #[error("artifact {artifact:?} has no certified native group {original_ordinal}")]
    NativeGroupUnavailable {
        artifact: ArtifactId,
        original_ordinal: u32,
    },
    #[error("{dependent:?} requires another exact interface seal for {required:?}")]
    InterfaceSealMismatch {
        dependent: ExactModuleIdentity,
        required: ExactModuleIdentity,
    },
    #[error("{dependent:?} requires unavailable {required:?} through {dependency:?}")]
    MissingDependency {
        artifact: ArtifactId,
        dependent: ExactModuleIdentity,
        required: ExactModuleIdentity,
        dependency: ArtifactDependency,
    },
}

pub struct ArtifactInventoryError {
    pub failure: ArtifactInventoryFailure,
    pub diagnostic_artifacts: Option<std::path::PathBuf>,
    pub(crate) owner_conflict: Option<ArtifactOwnerConflictEvidence>,
}

pub(crate) enum ArtifactOwnerConflictEvidence {
    ArtifactSelection {
        existing: Arc<ArtifactEntry>,
        incoming: Arc<ArtifactEntry>,
        existing_in_parent: bool,
    },
    NativeCanonicalCarrier {
        selected: Arc<ArtifactEntry>,
        incoming: Arc<ArtifactEntry>,
    },
    CompilerRole {
        existing: CompilerInputRole,
        incoming: CompilerInputRole,
    },
}

impl ArtifactOwnerConflictEvidence {
    fn summary(&self) -> String {
        fn artifact(entry: &ArtifactEntry) -> String {
            format!("{:?}:{}", entry.descriptor.kind, hex(entry.descriptor.id))
        }
        fn hex(id: ArtifactId) -> String {
            id.0.iter().map(|byte| format!("{byte:02x}")).collect()
        }
        fn role(role: &CompilerInputRole) -> String {
            let kind = match role {
                CompilerInputRole::InterfaceOnly { .. } => "interface_only",
                CompilerInputRole::ReusableOriginal { .. } => "reusable_original",
                CompilerInputRole::PublishedSourceOriginal { .. } => "published_source_original",
            };
            let mut summary = format!("{kind}:interface={}", hex(role.interface()));
            if let Some(original) = role.original() {
                summary.push_str(&format!(",original={}", hex(original)));
            }
            summary
        }
        match self {
            Self::ArtifactSelection {
                existing, incoming, ..
            } => format!(
                "artifact_selection existing={} incoming={}",
                artifact(existing),
                artifact(incoming)
            ),
            Self::NativeCanonicalCarrier { selected, incoming } => format!(
                "native_canonical_carrier existing={} incoming={}",
                artifact(selected),
                artifact(incoming)
            ),
            Self::CompilerRole { existing, incoming } => format!(
                "compiler_role existing={} incoming={}",
                role(existing),
                role(incoming)
            ),
        }
    }
    fn metadata(&self) -> serde_json::Value {
        match self {
            Self::ArtifactSelection {
                existing,
                incoming,
                existing_in_parent,
            } => serde_json::json!({
                "boundary": "artifact_selection",
                "existing": existing.descriptor,
                "incoming": incoming.descriptor,
                "existing_in_parent": existing_in_parent,
            }),
            Self::NativeCanonicalCarrier { selected, incoming } => serde_json::json!({
                "boundary": "native_canonical_carrier",
                "existing": selected.descriptor,
                "incoming": incoming.descriptor,
            }),
            Self::CompilerRole { existing, incoming } => serde_json::json!({
                "boundary": "compiler_role",
                "existing_role": existing,
                "incoming_role": incoming,
            }),
        }
    }

    fn entries(&self) -> Option<(&Arc<ArtifactEntry>, &Arc<ArtifactEntry>)> {
        match self {
            Self::ArtifactSelection {
                existing, incoming, ..
            } => Some((existing, incoming)),
            Self::NativeCanonicalCarrier { selected, incoming } => Some((selected, incoming)),
            Self::CompilerRole { .. } => None,
        }
    }
}

impl std::fmt::Debug for ArtifactInventoryError {
    fn fmt(&self, output: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        output
            .debug_struct("ArtifactInventoryError")
            .field("failure", &self.failure)
            .field("diagnostic_artifacts", &self.diagnostic_artifacts)
            .field(
                "owner_conflict",
                &self
                    .owner_conflict
                    .as_ref()
                    .map(ArtifactOwnerConflictEvidence::summary),
            )
            .finish()
    }
}

impl ArtifactInventoryError {
    /// Only immutable evidence from the refused admission is retained. Reading
    /// these diagnostics cannot admit either artifact or change its scope.
    pub(crate) fn retain_owner_conflict(&self, root: &std::path::Path) -> std::io::Result<()> {
        let Some(evidence) = &self.owner_conflict else {
            return Ok(());
        };
        let root = root.join("owner-conflict");
        std::fs::create_dir(&root)?;
        let metadata = evidence.metadata();
        std::fs::write(
            root.join("index.json"),
            serde_json::to_vec_pretty(&metadata).map_err(std::io::Error::other)?,
        )?;
        let Some((existing, incoming)) = evidence.entries() else {
            return Ok(());
        };
        for (label, entry) in [("existing", existing), ("incoming", incoming)] {
            if let ArtifactPayload::Canonical(interface) = &entry.payload {
                for (extension, bytes) in [
                    ("finalized.cbor", interface.certificate_bytes()),
                    ("hi", interface.interface_bytes()),
                    ("packages", interface.package_imports_bytes()),
                ] {
                    std::fs::write(root.join(format!("{label}.{extension}")), bytes)?;
                }
                if let Some(core) = interface.core_bytes() {
                    std::fs::write(root.join(format!("{label}.core")), core)?;
                }
            }
        }
        Ok(())
    }
}

impl std::fmt::Display for ArtifactInventoryError {
    fn fmt(&self, output: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.failure.fmt(output)?;
        if let Some(evidence) = &self.owner_conflict {
            write!(output, "; conflict evidence {}", evidence.summary())?;
        }
        if let Some(path) = &self.diagnostic_artifacts {
            write!(
                output,
                "; compiler artifacts retained at {}",
                path.display()
            )?;
        }
        Ok(())
    }
}

impl std::error::Error for ArtifactInventoryError {}

pub(crate) fn admission_failure(failure: ArtifactInventoryFailure) -> CompileError {
    ArtifactInventoryError {
        failure,
        diagnostic_artifacts: None,
        owner_conflict: None,
    }
    .into()
}

/// An exact live value required by original native code. Execution selection
/// and conservative authored-module lifetime custody query this separately.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct NativeBindingRequirement {
    pub artifact_id: ArtifactId,
    pub identity: tidepool_repr::execution_schema::SymbolIdentity,
    pub generation: u64,
}

/// A package export required by one original native group. Its interface
/// witness certifies the owner; activation still requires its exact live lease.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct RetainedPackageDependency {
    pub dependent_ordinal: u32,
    pub identity: tidepool_repr::execution_schema::SymbolIdentity,
    pub generation: u64,
    pub interface_digest: [u8; 32],
}

/// A selected external package obligation, rooted at its demanding original.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct NativePackageRequirement {
    pub artifact_id: ArtifactId,
    pub identity: tidepool_repr::execution_schema::SymbolIdentity,
    pub generation: u64,
    pub interface_digest: [u8; 32],
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct NativeRequirements {
    pub bindings: Vec<NativeBindingRequirement>,
    pub packages: Vec<NativePackageRequirement>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ArtifactDescriptor {
    pub id: ArtifactId,
    pub kind: ArtifactKind,
    pub owner: ExactModuleIdentity,
    pub producer_sha256: [u8; 32],
    pub interface_sha256: [u8; 32],
    pub product_sha256: Option<[u8; 32]>,
    pub package_imports_sha256: [u8; 32],
    pub certification_sha256: Option<[u8; 32]>,
}

impl ArtifactDescriptor {
    /// Describes persisted evidence; constructing a descriptor does not admit it.
    pub fn from_recovery_product(
        reference: &crate::recovery_artifacts::RecoveryArtifactRef,
    ) -> Self {
        descriptor(
            ArtifactKind::OriginalModule,
            ExactModuleIdentity {
                unit: reference.unit.clone(),
                module: reference.module.clone(),
            },
            reference.toolchain_identity_sha256,
            reference.skinny_iface_sha256,
            Some(reference.product_sha256),
            reference.package_imports_sha256,
            Some(reference.certification_sha256),
        )
    }
    pub fn from_recovery_module_interface(
        reference: &crate::recovery_artifacts::RecoveryModuleInterfaceRef,
    ) -> Self {
        let interface = &reference.interface;
        descriptor(
            ArtifactKind::CanonicalModuleInterface,
            ExactModuleIdentity {
                unit: interface.unit.clone(),
                module: interface.module.clone(),
            },
            interface.toolchain_identity_sha256,
            interface.skinny_iface_sha256,
            None,
            interface.package_imports_sha256,
            Some(reference.certificate_sha256),
        )
    }
    pub fn from_recovery_join(reference: &crate::recovery_artifacts::RecoveryJoinRef) -> Self {
        Self::from_recovery_interface(reference, JoinedInterfaceRole::LexicalJoin)
    }
    pub fn from_recovery_value_interface(
        reference: &crate::recovery_artifacts::RecoveryValueInterfaceRef,
    ) -> Self {
        Self::from_recovery_interface(&reference.interface, JoinedInterfaceRole::ValueInterface)
    }
    fn from_recovery_interface(
        reference: &crate::recovery_artifacts::RecoveryJoinRef,
        role: JoinedInterfaceRole,
    ) -> Self {
        descriptor(
            role.artifact_kind(),
            ExactModuleIdentity {
                unit: reference.unit.clone(),
                module: reference.module.clone(),
            },
            reference.toolchain_identity_sha256,
            reference.skinny_iface_sha256,
            None,
            reference.package_imports_sha256,
            None,
        )
    }
}

/// The two roles carried by a certified joined interface. Native products and
/// canonical module interfaces have their own payloads.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum JoinedInterfaceRole {
    LexicalJoin,
    ValueInterface,
}

impl JoinedInterfaceRole {
    fn artifact_kind(self) -> ArtifactKind {
        match self {
            Self::LexicalJoin => ArtifactKind::LexicalJoin,
            Self::ValueInterface => ArtifactKind::ValueInterface,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) enum ArtifactPayload {
    Original(CertifiedRecoveryProduct),
    Canonical(crate::certified_products::CertifiedModuleInterface),
    Interface(CertifiedJoinedInterface, JoinedInterfaceRole),
}

impl PartialEq for ArtifactPayload {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Original(left), Self::Original(right)) => left.same_durable_artifact(right),
            (Self::Canonical(left), Self::Canonical(right)) => left == right,
            (Self::Interface(left, left_role), Self::Interface(right, right_role)) => {
                left_role == right_role && left == right
            }
            _ => false,
        }
    }
}
impl Eq for ArtifactPayload {}

impl ArtifactPayload {
    pub(crate) fn artifact_kind(&self) -> ArtifactKind {
        match self {
            Self::Original(_) => ArtifactKind::OriginalModule,
            Self::Canonical(_) => ArtifactKind::CanonicalModuleInterface,
            Self::Interface(_, role) => role.artifact_kind(),
        }
    }
}

/// Immutable owner identity; canonicalization is local to this inventory.
#[derive(Clone, Debug)]
pub(crate) enum ArtifactFactsHandle {
    Original(Arc<crate::recovery_artifacts::OriginalFacts>),
    Canonical(Arc<crate::certified_products::CanonicalFacts>),
    Joined(Arc<JoinedFacts>),
}

impl PartialEq for ArtifactFactsHandle {
    fn eq(&self, other: &Self) -> bool {
        self.same_owner(other)
            || match (self, other) {
                (Self::Original(a), Self::Original(b)) => a == b,
                (Self::Canonical(a), Self::Canonical(b)) => a == b,
                (Self::Joined(a), Self::Joined(b)) => a == b,
                _ => false,
            }
    }
}
impl Eq for ArtifactFactsHandle {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct JoinedFacts {
    interface: CertifiedJoinedInterface,
    role: JoinedInterfaceRole,
    requirements: Vec<ExactModuleIdentity>,
}
impl ArtifactFactsHandle {
    fn same_owner(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Original(a), Self::Original(b)) => Arc::ptr_eq(a, b),
            (Self::Canonical(a), Self::Canonical(b)) => Arc::ptr_eq(a, b),
            (Self::Joined(a), Self::Joined(b)) => Arc::ptr_eq(a, b),
            _ => false,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ArtifactEntry {
    pub descriptor: ArtifactDescriptor,
    facts: ArtifactFactsHandle,
    pub payload: ArtifactPayload,
    pub requirements: Vec<ExactModuleIdentity>,
    interface_seals: BTreeMap<ExactModuleIdentity, [u8; 32]>,
    native: Option<Arc<crate::certified_products::OriginalNativeWitness>>,
}

#[derive(Debug, Default)]
pub(crate) struct NativeInventoryFacts {
    pub(crate) native_requirements: BTreeMap<u32, Vec<(ExactModuleIdentity, ArtifactDependency)>>,
    pub(crate) native_owners: BTreeMap<ExactModuleIdentity, NativeOwnerKey>,
    pub(crate) native_group_ordinals: BTreeSet<u32>,
    pub(crate) retained_packages: Vec<RetainedPackageDependency>,
}
impl std::ops::Deref for ArtifactEntry {
    type Target = NativeInventoryFacts;
    fn deref(&self) -> &NativeInventoryFacts {
        static EMPTY: OnceLock<NativeInventoryFacts> = OnceLock::new();
        self.native.as_ref().map_or_else(
            || EMPTY.get_or_init(Default::default),
            |native| native.inventory_facts(),
        )
    }
}
impl PartialEq for ArtifactEntry {
    fn eq(&self, other: &Self) -> bool {
        self.facts == other.facts
            && self.descriptor == other.descriptor
            && self.requirements == other.requirements
            && self.interface_seals == other.interface_seals
    }
}
impl Eq for ArtifactEntry {}

pub(crate) fn index_native_requirements(
    requirements: Vec<(ExactModuleIdentity, ArtifactDependency)>,
) -> Result<BTreeMap<u32, Vec<(ExactModuleIdentity, ArtifactDependency)>>, CompileError> {
    let mut indexed = BTreeMap::<u32, Vec<_>>::new();
    for (owner, dependency) in requirements {
        let ordinal = match &dependency {
            ArtifactDependency::NativeGroup {
                dependent_ordinal, ..
            }
            | ArtifactDependency::NativeBinding {
                dependent_ordinal, ..
            } => *dependent_ordinal,
            ArtifactDependency::Interface => {
                return Err(failure("interface edge in native group witness"));
            }
        };
        indexed
            .entry(ordinal)
            .or_default()
            .push((owner, dependency));
    }
    for requirements in indexed.values_mut() {
        requirements.sort();
        requirements.dedup();
    }
    Ok(indexed)
}

impl ArtifactEntry {
    pub(crate) fn finalize_joined_requirements(
        self,
        requirements: Vec<ExactModuleIdentity>,
    ) -> Result<Self, CompileError> {
        let ArtifactPayload::Interface(interface, role) = self.payload else {
            return Err(failure(
                "requirements finalization requires a joined interface",
            ));
        };
        Ok(Self::interface(interface, role, requirements))
    }

    fn adopt_facts(&self, facts: ArtifactFactsHandle) -> Self {
        let payload = match (&self.payload, &facts) {
            (ArtifactPayload::Original(product), ArtifactFactsHandle::Original(owner)) => {
                ArtifactPayload::Original(product.clone().with_facts(Arc::clone(owner)))
            }
            (ArtifactPayload::Canonical(interface), ArtifactFactsHandle::Canonical(owner)) => {
                ArtifactPayload::Canonical(interface.clone().with_facts(Arc::clone(owner)))
            }
            (ArtifactPayload::Interface(_, _), ArtifactFactsHandle::Joined(owner)) => {
                ArtifactPayload::Interface(owner.interface.clone(), owner.role)
            }
            _ => unreachable!("canonical facts variant"),
        };
        let mut adopted = self.clone();
        adopted.native = match &payload {
            ArtifactPayload::Original(product) => product.original_native().cloned(),
            _ => None,
        };
        adopted.payload = payload;
        adopted.facts = facts;
        adopted
    }
    fn catalog_input_custody(
        &self,
    ) -> Option<Arc<crate::module_candidates::deployment::DeploymentArtifactPaths>> {
        match &self.payload {
            ArtifactPayload::Canonical(interface) => interface.catalog_input_custody(),
            ArtifactPayload::Original(product) => product
                .module_interface()
                .and_then(|interface| interface.catalog_input_custody()),
            ArtifactPayload::Interface(_, _) => None,
        }
    }

    fn source_admission_witness(&self) -> Option<[u8; 32]> {
        match &self.payload {
            ArtifactPayload::Original(product) => product.source_sha256(),
            _ => None,
        }
    }

    fn same_issuing_authority(&self, other: &Self) -> bool {
        self.source_admission_witness() == other.source_admission_witness()
            && match (self.catalog_input_custody(), other.catalog_input_custody()) {
                (None, None) => true,
                (Some(first), Some(second)) => Arc::ptr_eq(&first, &second),
                _ => false,
            }
    }

    fn without_issuing_authority(entry: &Arc<Self>) -> Arc<Self> {
        if entry.catalog_input_custody().is_none() && entry.source_admission_witness().is_none() {
            return Arc::clone(entry);
        }
        let mut semantic = (**entry).clone();
        semantic.payload = match &entry.payload {
            ArtifactPayload::Canonical(interface) => {
                ArtifactPayload::Canonical(interface.clone().without_catalog_input_custody())
            }
            ArtifactPayload::Original(product) => {
                ArtifactPayload::Original(product.clone().without_issuing_authority())
            }
            payload => payload.clone(),
        };
        Arc::new(semantic)
    }

    fn merge_catalog_input_custody(
        previous: &Arc<Self>,
        entry: &Arc<Self>,
    ) -> Result<Arc<Self>, CompileError> {
        if previous != entry {
            return Err(admission_failure(
                ArtifactInventoryFailure::MetadataConflict {
                    artifact: entry.descriptor.id,
                },
            ));
        }
        let previous_custody = previous.catalog_input_custody();
        let incoming_custody = entry.catalog_input_custody();
        let Some(incoming) = incoming_custody else {
            return Ok(Arc::clone(previous));
        };
        if previous_custody
            .as_ref()
            .is_some_and(|retained| Arc::ptr_eq(retained, &incoming))
        {
            return Ok(Arc::clone(previous));
        }
        let payload = match (&previous.payload, &entry.payload) {
            (ArtifactPayload::Canonical(first), ArtifactPayload::Canonical(second)) => {
                ArtifactPayload::Canonical(first.merge_catalog_input_custody(second)?)
            }
            (ArtifactPayload::Original(first), ArtifactPayload::Original(second)) => {
                ArtifactPayload::Original(first.merge_catalog_input_custody(second)?)
            }
            _ => return Ok(Arc::clone(previous)),
        };
        let mut merged = (**previous).clone();
        merged.payload = payload;
        if previous_custody
            .as_ref()
            .zip(merged.catalog_input_custody().as_ref())
            .is_some_and(|(first, second)| Arc::ptr_eq(first, second))
        {
            return Ok(Arc::clone(previous));
        }
        Ok(Arc::new(merged))
    }

    fn native_requirements_for(
        &self,
        ordinal: u32,
    ) -> &[(ExactModuleIdentity, ArtifactDependency)] {
        self.native_requirements
            .get(&ordinal)
            .map_or(&[], Vec::as_slice)
    }

    #[cfg(test)]
    pub(crate) fn original(
        producer: [u8; 32],
        product: CertifiedRecoveryProduct,
    ) -> Result<Self, CompileError> {
        Self::original_with_validation(
            producer,
            product,
            &mut crate::recovery_artifacts::PackageInterfaceValidation::default(),
        )
    }

    pub(crate) fn original_with_validation(
        producer: [u8; 32],
        product: CertifiedRecoveryProduct,
        validation: &mut crate::recovery_artifacts::PackageInterfaceValidation,
    ) -> Result<Self, CompileError> {
        let product =
            crate::certified_products::finalize_original_for_inventory(product, validation)
                .map_err(|error| failure(&format!("original facts: {error}")))?;
        let canonical = product
            .module_interface()
            .ok_or_else(|| failure("native product lacks its canonical finalized module"))?;
        if canonical.producer_sha256() != producer {
            return Err(failure("native product has another canonical producer"));
        }
        let interface_seals =
            crate::certified_products::original_interface_requirements_with_operation(
                &product,
                &validation.inventory,
            )
            .map_err(|error| failure(&format!("original interface requirements: {error}")))?
            .into_iter()
            .map(|((unit, module), seal)| (ExactModuleIdentity { unit, module }, seal))
            .collect::<BTreeMap<_, _>>();
        // The canonical compiler interface owns type closure. Source lexical
        // adjacency cannot add or remove its sealed interface requirements.
        let requirements = interface_seals.keys().cloned().collect();
        let descriptor = Self::original_descriptor(producer, &product);
        Ok(Self {
            descriptor,
            facts: ArtifactFactsHandle::Original(Arc::clone(
                product.facts().expect("finalized original"),
            )),
            payload: ArtifactPayload::Original(product.clone()),
            requirements,
            interface_seals,
            native: product.original_native().cloned(),
        })
    }
    fn original_descriptor(
        producer: [u8; 32],
        product: &CertifiedRecoveryProduct,
    ) -> ArtifactDescriptor {
        let owner = product.owner();
        descriptor(
            ArtifactKind::OriginalModule,
            ExactModuleIdentity {
                unit: owner.unit.clone(),
                module: owner.module.clone(),
            },
            producer,
            owner.skinny_iface_sha256,
            Some(owner.product_sha256),
            digest(product.package_imports_bytes()),
            Some(digest(product.certification_bytes())),
        )
    }
    pub(crate) fn original_artifact_id(
        producer: [u8; 32],
        product: &CertifiedRecoveryProduct,
    ) -> ArtifactId {
        Self::original_descriptor(producer, product).id
    }
    pub(crate) fn canonical(
        interface: crate::certified_products::CertifiedModuleInterface,
    ) -> Self {
        let interface_seals = interface
            .requirements()
            .iter()
            .map(|((unit, module), seal)| {
                (
                    ExactModuleIdentity {
                        unit: unit.clone(),
                        module: module.clone(),
                    },
                    *seal,
                )
            })
            .collect::<BTreeMap<_, _>>();
        let descriptor = descriptor(
            ArtifactKind::CanonicalModuleInterface,
            ExactModuleIdentity {
                unit: interface.unit().into(),
                module: interface.module().into(),
            },
            interface.producer_sha256(),
            interface.interface_sha256(),
            None,
            interface.package_imports_sha256(),
            Some(digest(interface.certificate_bytes())),
        );
        Self {
            descriptor,
            facts: ArtifactFactsHandle::Canonical(Arc::clone(interface.facts())),
            payload: ArtifactPayload::Canonical(interface),
            requirements: interface_seals.keys().cloned().collect(),
            interface_seals,
            native: None,
        }
    }

    fn is_native(&self) -> bool {
        matches!(self.payload, ArtifactPayload::Original(_))
    }

    pub(crate) fn interface(
        interface: CertifiedJoinedInterface,
        role: JoinedInterfaceRole,
        mut requirements: Vec<ExactModuleIdentity>,
    ) -> Self {
        requirements.sort();
        requirements.dedup();
        let descriptor = interface.descriptor(role).clone();
        Self {
            descriptor,
            facts: ArtifactFactsHandle::Joined(Arc::new(JoinedFacts {
                interface: interface.clone(),
                role,
                requirements: requirements.clone(),
            })),
            payload: ArtifactPayload::Interface(interface, role),
            requirements,
            interface_seals: BTreeMap::new(),
            native: None,
        }
    }
}
pub(crate) fn restore_recovery_interface_dependencies(
    entries: &mut [ArtifactEntry],
    descriptors: &[ArtifactDescriptor],
    dependencies: &[(ArtifactId, ArtifactId, ArtifactDependency)],
) -> Result<(), CompileError> {
    use crate::artifact_inventory::ArtifactDependency;
    let expected = entries
        .iter()
        .map(|entry| (entry.descriptor.id, entry.descriptor.clone()))
        .collect::<BTreeMap<_, _>>();
    let supplied = descriptors
        .iter()
        .map(|descriptor| (descriptor.id, descriptor.clone()))
        .collect::<BTreeMap<_, _>>();
    if expected.len() != entries.len()
        || supplied.len() != descriptors.len()
        || supplied != expected
    {
        return Err(failure(
            "recovered artifact descriptor closure differs from certified bytes",
        ));
    }
    let edges = dependencies.iter().cloned().collect::<BTreeSet<_>>();
    if edges.len() != dependencies.len()
        || edges
            .iter()
            .any(|(from, to, _)| !expected.contains_key(from) || !expected.contains_key(to))
    {
        return Err(failure("invalid recovered dependency endpoints"));
    }
    if edges
        .iter()
        .any(|(_, _, dependency)| !matches!(dependency, ArtifactDependency::Interface))
    {
        return Err(failure(
            "native recovery dependencies must derive from original certification",
        ));
    }
    for entry in entries.iter_mut() {
        if let ArtifactPayload::Original(product) = &entry.payload {
            let canonical = ArtifactEntry::canonical(
                product
                    .module_interface()
                    .ok_or_else(|| failure("recovered native lacks canonical carrier"))?
                    .clone(),
            )
            .descriptor
            .id;
            let direct = edges
                .iter()
                .filter(|(from, _, _)| *from == entry.descriptor.id)
                .cloned()
                .collect::<Vec<_>>();
            if direct
                != vec![(
                    entry.descriptor.id,
                    canonical,
                    ArtifactDependency::Interface,
                )]
            {
                return Err(failure(
                    "native interface edge differs from canonical carrier",
                ));
            }
            continue;
        }
        let requirements = edges
            .iter()
            .filter(|(from, to, dependency)| {
                *from == entry.descriptor.id
                    && matches!(dependency, ArtifactDependency::Interface)
                    && expected[to].owner != entry.descriptor.owner
            })
            .map(|(_, to, _)| expected[to].owner.clone())
            .collect::<BTreeSet<_>>();
        if matches!(
            entry.descriptor.kind,
            ArtifactKind::ValueInterface | ArtifactKind::CanonicalModuleInterface
        ) && requirements != entry.requirements.iter().cloned().collect()
        {
            return Err(failure(
                "value interface dependencies differ from retained evidence",
            ));
        }
        if !entry
            .interface_seals
            .keys()
            .all(|owner| requirements.contains(owner))
        {
            return Err(failure(
                "recovered original omits certified interface requirements",
            ));
        }
        if matches!(entry.payload, ArtifactPayload::Interface(_, _)) {
            *entry = entry
                .clone()
                .finalize_joined_requirements(requirements.into_iter().collect())?;
        }
    }
    Ok(())
}

pub(crate) fn joined_descriptor(
    kind: ArtifactKind,
    owner: ExactModuleIdentity,
    producer: [u8; 32],
    interface: [u8; 32],
    packages: [u8; 32],
) -> ArtifactDescriptor {
    descriptor(kind, owner, producer, interface, None, packages, None)
}

fn descriptor(
    kind: ArtifactKind,
    owner: ExactModuleIdentity,
    producer_sha256: [u8; 32],
    interface_sha256: [u8; 32],
    product_sha256: Option<[u8; 32]>,
    package_imports_sha256: [u8; 32],
    certification_sha256: Option<[u8; 32]>,
) -> ArtifactDescriptor {
    let mut descriptor = ArtifactDescriptor {
        id: ArtifactId([0; 32]),
        kind,
        owner,
        producer_sha256,
        interface_sha256,
        product_sha256,
        package_imports_sha256,
        certification_sha256,
    };
    descriptor.id = ArtifactId(digest(
        &serde_json::to_vec(&descriptor).expect("artifact descriptor encoding"),
    ));
    descriptor
}
fn digest(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}
fn failure(message: &str) -> CompileError {
    CompileError::ExtractFailed(format!("artifact inventory: {message}"))
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub(crate) struct NativeOwnerKey {
    owner: ExactModuleIdentity,
    version: [u8; 32],
    interface: [u8; 32],
    product: [u8; 32],
}
impl NativeOwnerKey {
    pub(crate) fn from_owner(owner: &tidepool_repr::execution_schema::CachedHomeOwner) -> Self {
        Self {
            owner: ExactModuleIdentity {
                unit: owner.unit.clone(),
                module: owner.module.clone(),
            },
            version: owner.module_version.0,
            interface: owner.skinny_iface_sha256,
            product: owner.product_sha256,
        }
    }
}

#[derive(Default)]
struct InventoryState {
    graph: StableDiGraph<InventoryNodeKey, ArtifactDependency>,
    indices: BTreeMap<InventoryNodeKey, NodeIndex>,
    payloads: BTreeMap<ArtifactId, Arc<ArtifactEntry>>,
    payload_bindings: BTreeMap<ArtifactId, usize>,
    /// Immutable proof evidence lasts for this inventory's lifetime. Reclaiming
    /// native payload custody does not collect proof-only ancestor rows.
    selection_plans: BTreeMap<ArtifactSelectionId, Arc<ScopeProofV3>>,
    roots: BTreeMap<InventoryNodeKey, usize>,
    graph_visits: AtomicU64,
    view_queries: AtomicU64,
    entry_handle_copies: AtomicU64,
    admission_owner_lookups: AtomicU64,
    canonical_hits: AtomicU64,
    canonical_comparisons: AtomicU64,
    canonical_bytes_offered: AtomicU64,
    installed_binding_rows: AtomicU64,
    #[cfg(test)]
    native_requirement_rows_examined: AtomicU64,
    #[cfg(test)]
    dependency_graph_visits: AtomicU64,
    reclamation_runs: u64,
    reclamation_candidate_nodes: u64,
    reclaimed_nodes: u64,
    reclamation_elapsed_ns: u64,
}

impl InventoryState {
    fn canonicalize(&self, entry: Arc<ArtifactEntry>) -> Result<Arc<ArtifactEntry>, CompileError> {
        let consistent = match (&entry.facts, &entry.payload) {
            (ArtifactFactsHandle::Original(owner), ArtifactPayload::Original(product)) => product
                .facts()
                .is_some_and(|facts| Arc::ptr_eq(owner, facts)),
            (ArtifactFactsHandle::Canonical(owner), ArtifactPayload::Canonical(interface)) => {
                Arc::ptr_eq(owner, interface.facts())
            }
            (ArtifactFactsHandle::Joined(owner), ArtifactPayload::Interface(interface, role)) => {
                owner.role == *role
                    && owner.requirements == entry.requirements
                    && owner.interface == *interface
            }
            _ => false,
        };
        if !consistent {
            return Err(admission_failure(
                ArtifactInventoryFailure::MetadataConflict {
                    artifact: entry.descriptor.id,
                },
            ));
        }
        if self
            .payloads
            .get(&entry.descriptor.id)
            .is_some_and(|resident| resident.facts.same_owner(&entry.facts))
        {
            self.canonical_hits.fetch_add(1, Ordering::Relaxed);
            return Ok(entry);
        }
        let entry = if let ArtifactPayload::Original(product) = &entry.payload {
            let interface = product
                .module_interface()
                .expect("complete canonical companion");
            let normalized =
                self.canonicalize(Arc::new(ArtifactEntry::canonical(interface.clone())))?;
            let ArtifactFactsHandle::Canonical(canonical) = &normalized.facts else {
                unreachable!()
            };
            if Arc::ptr_eq(interface.facts(), canonical) {
                entry
            } else {
                let product = product.clone().with_canonical_facts(Arc::clone(canonical));
                Arc::new(entry.adopt_facts(ArtifactFactsHandle::Original(Arc::clone(
                    product.facts().unwrap(),
                ))))
            }
        } else {
            entry
        };
        let Some(existing) = self.payloads.get(&entry.descriptor.id) else {
            return Ok(entry);
        };
        // Privately constructed entries keep facade custody separate while
        // sharing the complete immutable owner.
        if existing.facts.same_owner(&entry.facts) {
            self.canonical_hits.fetch_add(1, Ordering::Relaxed);
            return Ok(entry);
        }
        self.canonical_comparisons.fetch_add(1, Ordering::Relaxed);
        let offered = match &entry.payload {
            ArtifactPayload::Original(product) => product
                .original_byte_anchors()
                .iter()
                .map(|bytes| bytes.len())
                .sum::<usize>(),
            ArtifactPayload::Canonical(interface) => {
                interface.interface_bytes().len()
                    + interface.package_imports_bytes().len()
                    + interface.certificate_bytes().len()
                    + interface.core_bytes().map_or(0, <[u8]>::len)
            }
            ArtifactPayload::Interface(interface, _) => {
                interface.interface_bytes().len() + interface.package_imports_bytes().len()
            }
        };
        self.canonical_bytes_offered
            .fetch_add(offered as u64, Ordering::Relaxed);
        if existing != &entry {
            return Err(admission_failure(
                ArtifactInventoryFailure::MetadataConflict {
                    artifact: entry.descriptor.id,
                },
            ));
        }
        Ok(Arc::new(entry.adopt_facts(existing.facts.clone())))
    }
}

/// Owner selection belongs to one sealed reachable closure, not the inventory.
#[derive(Default)]
struct SelectedOwners {
    interfaces: BTreeMap<ExactModuleIdentity, ArtifactId>,
    native: BTreeMap<NativeOwnerKey, ArtifactId>,
}
impl SelectedOwners {
    fn new(
        state: &InventoryState,
        entries: &BTreeMap<ArtifactId, Arc<ArtifactEntry>>,
        parent_ids: &BTreeSet<ArtifactId>,
    ) -> Result<Self, CompileError> {
        let mut selected = Self::default();
        for (id, entry) in entries {
            state
                .admission_owner_lookups
                .fetch_add(1, Ordering::Relaxed);
            let previous = if let ArtifactPayload::Original(product) = &entry.payload {
                selected
                    .native
                    .insert(NativeOwnerKey::from_owner(product.owner()), *id)
            } else {
                selected
                    .interfaces
                    .insert(entry.descriptor.owner.clone(), *id)
            };
            if let Some(previous) = previous.filter(|previous| previous != id) {
                let (existing, incoming) =
                    if parent_ids.contains(id) && !parent_ids.contains(&previous) {
                        (entry, &entries[&previous])
                    } else {
                        (&entries[&previous], entry)
                    };
                return Err(ArtifactInventoryError {
                    failure: ArtifactInventoryFailure::OwnerConflict {
                        owner: entry.descriptor.owner.clone(),
                    },
                    diagnostic_artifacts: None,
                    owner_conflict: Some(ArtifactOwnerConflictEvidence::ArtifactSelection {
                        existing: Arc::clone(existing),
                        incoming: Arc::clone(incoming),
                        existing_in_parent: parent_ids.contains(&existing.descriptor.id),
                    }),
                }
                .into());
            }
        }
        Ok(selected)
    }
    fn planned_edges(
        &self,
        state: &InventoryState,
        entries: &BTreeMap<ArtifactId, Arc<ArtifactEntry>>,
        groups: &BTreeSet<NativeGroupKey>,
    ) -> Result<
        (
            BTreeMap<LogicalNodeKey, BTreeSet<(LogicalNodeKey, ArtifactDependency)>>,
            BTreeSet<NativeGroupKey>,
        ),
        CompileError,
    > {
        let mut planned = BTreeMap::new();
        for (id, entry) in entries {
            planned.insert(
                LogicalNodeKey::Artifact(*id),
                self.dependencies(state, entry, entries)?
                    .into_iter()
                    .map(|(id, edge)| (LogicalNodeKey::Artifact(id), edge))
                    .collect(),
            );
        }
        let mut pending = groups.iter().copied().collect::<Vec<_>>();
        while let Some(key) = pending.pop() {
            let node = LogicalNodeKey::Group(key);
            if planned.contains_key(&node) {
                continue;
            }
            let entry = entries.get(&key.artifact).ok_or_else(|| {
                admission_failure(ArtifactInventoryFailure::NativeRootOutsideView {
                    artifact: key.artifact,
                })
            })?;
            if !entry.native_group_ordinals.contains(&key.original_ordinal) {
                return Err(admission_failure(
                    ArtifactInventoryFailure::NativeGroupUnavailable {
                        artifact: key.artifact,
                        original_ordinal: key.original_ordinal,
                    },
                ));
            }
            let mut edges = BTreeSet::from([(
                LogicalNodeKey::Artifact(key.artifact),
                ArtifactDependency::Interface,
            )]);
            for (owner, dependency) in entry.native_requirements_for(key.original_ordinal) {
                #[cfg(test)]
                state
                    .native_requirement_rows_examined
                    .fetch_add(1, Ordering::Relaxed);
                state
                    .admission_owner_lookups
                    .fetch_add(1, Ordering::Relaxed);
                let id = match dependency {
                    ArtifactDependency::NativeGroup { .. } => entry
                        .native_owners
                        .get(owner)
                        .and_then(|owner| self.native.get(owner)),
                    _ => self.interfaces.get(owner),
                }
                .copied()
                .ok_or_else(|| {
                    admission_failure(ArtifactInventoryFailure::MissingDependency {
                        artifact: key.artifact,
                        dependent: entry.descriptor.owner.clone(),
                        required: owner.clone(),
                        dependency: dependency.clone(),
                    })
                })?;
                let target = match dependency {
                    ArtifactDependency::NativeGroup {
                        required_ordinal, ..
                    } => {
                        let required = NativeGroupKey {
                            artifact: id,
                            original_ordinal: *required_ordinal,
                        };
                        pending.push(required);
                        LogicalNodeKey::Group(required)
                    }
                    _ => LogicalNodeKey::Artifact(id),
                };
                edges.insert((target, dependency.clone()));
            }
            planned.insert(node, edges);
        }
        let closed_groups = planned
            .keys()
            .filter_map(|key| match key {
                LogicalNodeKey::Group(key) => Some(*key),
                _ => None,
            })
            .collect();
        Ok((planned, closed_groups))
    }
    fn dependencies(
        &self,
        state: &InventoryState,
        entry: &ArtifactEntry,
        entries: &BTreeMap<ArtifactId, Arc<ArtifactEntry>>,
    ) -> Result<BTreeSet<(ArtifactId, ArtifactDependency)>, CompileError> {
        let canonical = match &entry.payload {
            ArtifactPayload::Canonical(interface) => Some(interface),
            ArtifactPayload::Original(product) => Some(
                product
                    .module_interface()
                    .ok_or_else(|| failure("native canonical carrier missing"))?,
            ),
            ArtifactPayload::Interface(_, _) => None,
        };
        if let Some(canonical) = canonical {
            let sealed = canonical
                .requirements()
                .iter()
                .map(|((unit, module), seal)| {
                    (
                        ExactModuleIdentity {
                            unit: unit.clone(),
                            module: module.clone(),
                        },
                        *seal,
                    )
                })
                .collect::<BTreeMap<_, _>>();
            if entry.interface_seals != sealed
                || entry.requirements.iter().collect::<BTreeSet<_>>() != sealed.keys().collect()
            {
                return Err(admission_failure(
                    ArtifactInventoryFailure::MetadataConflict {
                        artifact: entry.descriptor.id,
                    },
                ));
            }
        }
        let resolve = |owner: &ExactModuleIdentity, dependency: &ArtifactDependency| {
            state
                .admission_owner_lookups
                .fetch_add(1, Ordering::Relaxed);
            let id = if matches!(dependency, ArtifactDependency::NativeGroup { .. }) {
                entry
                    .native_owners
                    .get(owner)
                    .and_then(|key| self.native.get(key))
            } else {
                self.interfaces.get(owner)
            };
            id.copied().ok_or_else(|| {
                admission_failure(ArtifactInventoryFailure::MissingDependency {
                    artifact: entry.descriptor.id,
                    dependent: entry.descriptor.owner.clone(),
                    required: owner.clone(),
                    dependency: dependency.clone(),
                })
            })
        };
        for (owner, seal) in &entry.interface_seals {
            let id = resolve(owner, &ArtifactDependency::Interface)?;
            let required = &entries[&id].descriptor;
            if required.interface_sha256 != *seal
                || required.producer_sha256 != entry.descriptor.producer_sha256
            {
                return Err(admission_failure(
                    ArtifactInventoryFailure::InterfaceSealMismatch {
                        dependent: entry.descriptor.owner.clone(),
                        required: owner.clone(),
                    },
                ));
            }
        }
        let mut edges = BTreeSet::new();
        for owner in &entry.requirements {
            let id = resolve(owner, &ArtifactDependency::Interface)?;
            if !entry.is_native() {
                edges.insert((id, ArtifactDependency::Interface));
            }
        }
        if let ArtifactPayload::Original(product) = &entry.payload {
            let carrier = ArtifactEntry::canonical(
                product
                    .module_interface()
                    .ok_or_else(|| failure("native canonical carrier missing"))?
                    .clone(),
            );
            let id = resolve(&entry.descriptor.owner, &ArtifactDependency::Interface)?;
            if id != carrier.descriptor.id {
                return Err(ArtifactInventoryError {
                    failure: ArtifactInventoryFailure::OwnerConflict {
                        owner: entry.descriptor.owner.clone(),
                    },
                    diagnostic_artifacts: None,
                    owner_conflict: Some(ArtifactOwnerConflictEvidence::NativeCanonicalCarrier {
                        selected: Arc::clone(&entries[&id]),
                        incoming: Arc::new(carrier),
                    }),
                }
                .into());
            }
            edges.insert((id, ArtifactDependency::Interface));
        }
        Ok(edges)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ArtifactInventoryMetrics {
    pub nodes: usize,
    pub graph_visits: u64,
    pub view_queries: u64,
    pub entry_handle_copies: u64,
    /// Structural invariant of this shared owner, not an activity measurement.
    #[serde(rename = "structural_whole_graph_copies")]
    pub whole_graph_copies: u64,
    pub reclamation_runs: u64,
    pub reclamation_candidate_nodes: u64,
    pub reclaimed_nodes: u64,
    /// Existing reclamation traversal/removal work; excludes parent destruction.
    pub reclamation_elapsed_ns: u64,
    pub admission_owner_lookups: u64,
    pub canonical_hits: u64,
    pub canonical_comparisons: u64,
    pub canonical_bytes_offered: u64,
    pub installed_binding_rows: u64,
}

/// A complete, validated insertion. Commit consumes it under the same graph
/// guard; existing rows are never mutated.
struct BindingCommit {
    entries: BTreeMap<ArtifactId, Arc<ArtifactEntry>>,
    additions: BTreeMap<InventoryNodeKey, BTreeSet<(InventoryNodeKey, ArtifactDependency)>>,
}
impl BindingCommit {
    fn prepare(
        state: &InventoryState,
        entries: &BTreeMap<ArtifactId, Arc<ArtifactEntry>>,
        plan: &BTreeMap<InventoryNodeKey, BTreeSet<(InventoryNodeKey, ArtifactDependency)>>,
    ) -> Result<Self, CompileError> {
        for (id, entry) in entries {
            if state
                .payloads
                .get(id)
                .is_some_and(|previous| !previous.facts.same_owner(&entry.facts))
            {
                return Err(admission_failure(
                    ArtifactInventoryFailure::MetadataConflict { artifact: *id },
                ));
            }
        }
        let mut additions = BTreeMap::new();
        for (key, edges) in plan {
            if let Some(index) = state.indices.get(key) {
                let actual = state
                    .graph
                    .edges(*index)
                    .map(|edge| (state.graph[edge.target()], edge.weight().clone()))
                    .collect::<BTreeSet<_>>();
                if &actual != edges {
                    return Err(admission_failure(
                        ArtifactInventoryFailure::MetadataConflict {
                            artifact: key.artifact(),
                        },
                    ));
                }
            } else {
                additions.insert(*key, edges.clone());
            }
            for (target, _) in edges {
                if !plan.contains_key(target) && !state.indices.contains_key(target) {
                    return Err(failure("binding dependency outside issued graph"));
                }
            }
        }
        Ok(Self {
            entries: entries.clone(),
            additions,
        })
    }
    fn commit(self, state: &mut InventoryState) {
        for key in self.additions.keys() {
            state.installed_binding_rows.fetch_add(1, Ordering::Relaxed);
            let index = state.graph.add_node(*key);
            state.indices.insert(*key, index);
            if matches!(key, InventoryNodeKey::Artifact(_)) {
                *state.payload_bindings.entry(key.artifact()).or_default() += 1;
            }
        }
        for (key, edges) in self.additions {
            for (target, edge) in edges {
                state
                    .graph
                    .add_edge(state.indices[&key], state.indices[&target], edge);
            }
            state.payloads.entry(key.artifact()).or_insert_with(|| {
                ArtifactEntry::without_issuing_authority(&self.entries[&key.artifact()])
            });
        }
    }
}

/// Private physical-segment custody. It never becomes an item view wholesale.
/// Namespace and native owner choice are fixed by the compiler's certified roles.
pub(crate) struct BoundArtifactSelection {
    view: ArtifactView,
    entries: BTreeMap<ArtifactId, Arc<ArtifactEntry>>,
    owners: SelectedOwners,
    captured_facts: BTreeMap<ArtifactId, ArtifactFactsHandle>,
    materializations: Vec<Arc<crate::declaration_context::RetainedArtifactMaterialization>>,
    groups: BTreeMap<NativeGroupKey, BTreeSet<NativeGroupBinding>>,
}

impl BoundArtifactSelection {
    fn resolve_binding(
        &self,
        id: ArtifactId,
        state: &InventoryState,
    ) -> Result<ArtifactBinding, CompileError> {
        self.view
            .resolve_binding(id, &self.view.read_projection(state).nodes)
    }

    fn target_groups(
        &self,
        imports: &[crate::certified_products::PendingImportOwner],
    ) -> Result<BTreeSet<NativeGroupKey>, CompileError> {
        let mut groups = BTreeSet::new();
        for import in imports {
            let crate::certified_products::PendingImportOwner::Source {
                owner,
                original_ordinal,
                binder,
            } = import
            else {
                continue;
            };
            let id = self
                .owners
                .native
                .get(&NativeOwnerKey::from_owner(owner))
                .ok_or_else(|| failure("certified target native membership"))?;
            let entry = &self.entries[id];
            let ArtifactPayload::Original(product) = &entry.payload else {
                return Err(failure("certified target native membership"));
            };
            if product.owner() != owner
                || !crate::certified_products::authenticates_original_native_entry(
                    product,
                    *original_ordinal,
                    binder,
                )
            {
                return Err(failure("certified target native membership"));
            }
            groups.insert(NativeGroupKey {
                artifact: *id,
                original_ordinal: *original_ordinal,
            });
        }
        Ok(groups)
    }
}

/// A run/context family owns one inventory, without a global registry.
#[derive(Clone, Default)]
pub struct ArtifactInventory(Arc<Mutex<InventoryState>>);
impl std::fmt::Debug for ArtifactInventory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ArtifactInventory")
            .field(
                "nodes",
                &self.0.lock().expect("inventory lock").graph.node_count(),
            )
            .finish()
    }
}
impl ArtifactInventory {
    pub fn empty_view(&self) -> ArtifactView {
        self.retain(Vec::new(), Vec::new(), Vec::new(), Vec::new())
    }
    fn retain(
        &self,
        roots: Vec<InventoryNodeKey>,
        custody_roots: Vec<ArtifactBindingNode>,
        parents: Vec<Arc<ViewLease>>,
        materialization_parents: Vec<
            Arc<crate::declaration_context::RetainedArtifactMaterialization>,
        >,
    ) -> ArtifactView {
        let mut state = self.0.lock().expect("inventory lock");
        for id in roots.iter().copied().chain(custody_roots.iter().copied()) {
            *state.roots.entry(id).or_default() += 1;
        }
        let selection = selection_snapshot_for_lease(
            &state,
            &BTreeMap::new(),
            &roots,
            &custody_roots,
            &parents,
        );
        drop(state);
        ArtifactView::new(ViewLease {
            selection,
            inventory: self.clone(),
            roots,
            custody_roots,
            parents,
            issuing_payloads: BTreeMap::new(),
            materialization_parents,
            materialization: Mutex::new(BTreeMap::new()),
        })
    }
    pub(crate) fn admit(
        &self,
        parent: &ArtifactView,
        entries: Vec<ArtifactEntry>,
    ) -> Result<ArtifactView, CompileError> {
        let entries = entries
            .into_iter()
            .map(|mut entry| {
                entry.requirements.sort();
                entry.requirements.dedup();

                Arc::new(entry)
            })
            .collect();
        self.admit_shared(parent, entries)
    }

    /// Ordinary whole-product consumers demand the complete certified census.
    pub(crate) fn admit_shared(
        &self,
        parent: &ArtifactView,
        entries: Vec<Arc<ArtifactEntry>>,
    ) -> Result<ArtifactView, CompileError> {
        self.admit_shared_with_demand(parent, entries, NativeArtifactDemand::AllGroups)
    }
    pub(crate) fn admit_shared_with_demand(
        &self,
        parent: &ArtifactView,
        entries: Vec<Arc<ArtifactEntry>>,
        demand: NativeArtifactDemand<'_>,
    ) -> Result<ArtifactView, CompileError> {
        self.admit_shared_with_intent(
            parent,
            entries,
            demand,
            ArtifactRootIntent::SuppliedArtifacts,
        )
    }
    pub(crate) fn admit_certified_with_demand(
        &self,
        parent: &ArtifactView,
        entries: Vec<Arc<ArtifactEntry>>,
        demand: NativeArtifactDemand<'_>,
        selection: &crate::certified_products::CertifiedSourceSelection,
    ) -> Result<ArtifactView, CompileError> {
        self.admit_shared_with_intent(
            parent,
            entries,
            demand,
            ArtifactRootIntent::Certified(selection),
        )
    }
    pub(crate) fn bind_segment(
        &self,
        parent: &ArtifactView,
        entries: Vec<Arc<ArtifactEntry>>,
        selection: &crate::certified_products::CertifiedSourceSelection,
    ) -> Result<BoundArtifactSelection, CompileError> {
        let captured_facts = entries
            .iter()
            .map(|entry| (entry.descriptor.id, entry.facts.clone()))
            .collect();
        let view = self.admit_certified_with_demand(
            parent,
            entries,
            NativeArtifactDemand::ScopeInterfaces,
            selection,
        )?;
        let entries = view
            .entries()
            .into_iter()
            .map(|entry| (entry.descriptor.id, entry))
            .collect::<BTreeMap<_, _>>();
        let graph = view.capture_graph_selection();
        let state = self.0.lock().expect("inventory lock");
        let namespace_entries = view
            .namespace()
            .keys()
            .map(|id| (*id, Arc::clone(&entries[id])))
            .collect();
        let mut owners = SelectedOwners::new(&state, &namespace_entries, &BTreeSet::new())?;
        // Freeze one checked full owner for each native reference. Historical
        // custody remains admissible; an ambiguous resolver cannot be issued.
        let native_entries = entries
            .iter()
            .filter(|(_, entry)| matches!(entry.payload, ArtifactPayload::Original(_)))
            .map(|(id, entry)| (*id, Arc::clone(entry)))
            .collect();
        let parent_ids = artifact_ids(&parent.read_projection(&state).nodes);
        owners.native = SelectedOwners::new(&state, &native_entries, &parent_ids)?.native;
        let mut groups = BTreeMap::<NativeGroupKey, BTreeSet<NativeGroupBinding>>::new();
        for group in &graph.native_groups {
            groups
                .entry(NativeGroupKey {
                    artifact: group.binding.artifact,
                    original_ordinal: group.original_ordinal,
                })
                .or_default()
                .insert(*group);
        }
        // The issuing role may qualify a group among several retained bindings.
        // Availability by itself leaves ambiguity intact until demand.
        let mut qualified = BTreeMap::<NativeGroupKey, BTreeSet<NativeGroupBinding>>::new();
        for requirement in selection.native_original_requirements() {
            for root in &requirement.selection.graph_selection().roots {
                let ArtifactBindingNode::Group(group) = root else {
                    continue;
                };
                if group.binding.artifact != requirement.original {
                    continue;
                }
                qualified
                    .entry(NativeGroupKey {
                        artifact: group.binding.artifact,
                        original_ordinal: group.original_ordinal,
                    })
                    .or_default()
                    .insert(*group);
            }
        }
        groups.extend(qualified);
        drop(state);
        let mut materializations = Vec::new();
        view.collect_materializations(&mut materializations, &mut BTreeSet::new())?;
        Ok(BoundArtifactSelection {
            view,
            entries,
            owners,
            captured_facts,
            materializations,
            groups,
        })
    }

    pub(crate) fn admit_bound_target(
        &self,
        fixed: &mut BoundArtifactSelection,
        cursor: &ArtifactView,
        demand: NativeArtifactDemand<'_>,
        visible_values: Vec<Arc<ArtifactEntry>>,
        selection: &crate::certified_products::CertifiedSourceSelection,
    ) -> Result<ArtifactView, CompileError> {
        if !Arc::ptr_eq(&self.0, &cursor.lease.inventory.0)
            || !Arc::ptr_eq(&self.0, &fixed.view.lease.inventory.0)
        {
            return Err(failure("bound target belongs to another inventory"));
        }
        let mut allowed_values = BTreeSet::new();
        for entry in visible_values {
            let Some(issued) = fixed.entries.get(&entry.descriptor.id) else {
                return Err(failure("visible output was not captured by this segment"));
            };
            if !issued.facts.same_owner(&entry.facts)
                && !fixed
                    .captured_facts
                    .get(&entry.descriptor.id)
                    .is_some_and(|captured| captured.same_owner(&entry.facts))
            {
                return Err(failure("visible output has another immutable owner"));
            }
            allowed_values.insert(entry.descriptor.id);
        }
        let demanded = match demand {
            NativeArtifactDemand::ScopeInterfaces => BTreeSet::new(),
            NativeArtifactDemand::AllGroups => fixed
                .entries
                .values()
                .flat_map(|entry| {
                    entry
                        .native_group_ordinals
                        .iter()
                        .map(move |ordinal| NativeGroupKey {
                            artifact: entry.descriptor.id,
                            original_ordinal: *ordinal,
                        })
                })
                .collect(),
            NativeArtifactDemand::VerifiedTarget { entry, imports } => {
                let mut groups = fixed.target_groups(imports)?;
                groups.insert(entry.native_group_key());
                groups
            }
            NativeArtifactDemand::CertifiedTargetImports(imports) => {
                fixed.target_groups(imports)?
            }
        };
        let namespace_ids = selection
            .artifact_ids_from_bound_entries(
                &fixed.entries,
                &fixed.owners.native,
                cursor,
                &allowed_values,
            )
            .map_err(|error| CompileError::CompilerEvidence(Box::new(error)))?;
        let mut materialization_parents = fixed.materializations.clone();
        cursor.collect_cached_materializations(&mut materialization_parents)?;
        let mut state = self.0.lock().expect("inventory lock");
        let cursor_projection = cursor.read_projection(&state);
        let cursor_nodes = &cursor_projection.nodes;
        let mut planned =
            BTreeMap::<NativeGroupKey, BTreeSet<(LogicalNodeKey, ArtifactDependency)>>::new();
        let mut pending = demanded.iter().copied().collect::<Vec<_>>();
        let mut reused = BTreeSet::new();
        while let Some(group) = pending.pop() {
            if let Some(issued) = fixed.groups.get(&group) {
                if issued.len() != 1 {
                    return Err(admission_failure(
                        ArtifactInventoryFailure::BindingAmbiguity {
                            artifact: group.artifact,
                        },
                    ));
                }
                reused.insert(group);
                continue;
            }
            if planned.contains_key(&group) {
                continue;
            }
            let entry = fixed.entries.get(&group.artifact).ok_or_else(|| {
                admission_failure(ArtifactInventoryFailure::NativeRootOutsideView {
                    artifact: group.artifact,
                })
            })?;
            if !entry
                .native_group_ordinals
                .contains(&group.original_ordinal)
            {
                return Err(admission_failure(
                    ArtifactInventoryFailure::NativeGroupUnavailable {
                        artifact: group.artifact,
                        original_ordinal: group.original_ordinal,
                    },
                ));
            }
            let mut edges = BTreeSet::from([(
                LogicalNodeKey::Artifact(group.artifact),
                ArtifactDependency::Interface,
            )]);
            for (owner, dependency) in entry.native_requirements_for(group.original_ordinal) {
                state
                    .admission_owner_lookups
                    .fetch_add(1, Ordering::Relaxed);
                #[cfg(test)]
                state
                    .native_requirement_rows_examined
                    .fetch_add(1, Ordering::Relaxed);
                let id = match dependency {
                    ArtifactDependency::NativeGroup { .. } => entry
                        .native_owners
                        .get(owner)
                        .and_then(|key| fixed.owners.native.get(key)),
                    _ => fixed.owners.interfaces.get(owner),
                }
                .copied()
                .ok_or_else(|| {
                    admission_failure(ArtifactInventoryFailure::MissingDependency {
                        artifact: group.artifact,
                        dependent: entry.descriptor.owner.clone(),
                        required: owner.clone(),
                        dependency: dependency.clone(),
                    })
                })?;
                let target = match dependency {
                    ArtifactDependency::NativeGroup {
                        required_ordinal, ..
                    } => {
                        let required = NativeGroupKey {
                            artifact: id,
                            original_ordinal: *required_ordinal,
                        };
                        pending.push(required);
                        LogicalNodeKey::Group(required)
                    }
                    _ => LogicalNodeKey::Artifact(id),
                };
                edges.insert((target, dependency.clone()));
            }
            planned.insert(group, edges);
        }
        let provisional = ArtifactSelectionId([0; 32]);
        let resolve = |key: LogicalNodeKey,
                       scope: ArtifactSelectionId|
         -> Result<InventoryNodeKey, CompileError> {
            Ok(match key {
                LogicalNodeKey::Artifact(id) => {
                    InventoryNodeKey::Artifact(fixed.resolve_binding(id, &state)?)
                }
                LogicalNodeKey::Group(group) => match fixed.groups.get(&group) {
                    Some(issued) if issued.len() == 1 => {
                        InventoryNodeKey::Group(*issued.first().unwrap())
                    }
                    Some(_) => {
                        return Err(admission_failure(
                            ArtifactInventoryFailure::BindingAmbiguity {
                                artifact: group.artifact,
                            },
                        ));
                    }
                    None => LogicalNodeKey::Group(group).bind(scope),
                },
            })
        };
        let provisional_plan = planned
            .iter()
            .map(|(group, edges)| {
                Ok((
                    LogicalNodeKey::Group(*group).bind(provisional),
                    edges
                        .iter()
                        .map(|(target, edge)| Ok((resolve(*target, provisional)?, edge.clone())))
                        .collect::<Result<BTreeSet<_>, CompileError>>()?,
                ))
            })
            .collect::<Result<BTreeMap<_, _>, CompileError>>()?;
        let witness = ScopeProofV3::issue(&provisional_plan, provisional).map(Arc::new);
        let scope = witness
            .as_ref()
            .map_or(provisional, |proof| proof.selection);
        let bound = planned
            .iter()
            .map(|(group, edges)| {
                Ok((
                    LogicalNodeKey::Group(*group).bind(scope),
                    edges
                        .iter()
                        .map(|(target, edge)| Ok((resolve(*target, scope)?, edge.clone())))
                        .collect::<Result<BTreeSet<_>, CompileError>>()?,
                ))
            })
            .collect::<Result<BTreeMap<_, _>, CompileError>>()?;
        let namespace = namespace_ids
            .iter()
            .map(|id| Ok((*id, fixed.resolve_binding(*id, &state)?)))
            .collect::<Result<BTreeMap<_, _>, CompileError>>()?;
        let new_roots = namespace
            .values()
            .copied()
            .map(InventoryNodeKey::Artifact)
            .chain(
                demanded
                    .iter()
                    .map(|group| resolve(LogicalNodeKey::Group(*group), scope))
                    .collect::<Result<Vec<_>, _>>()?,
            )
            .collect::<BTreeSet<_>>();
        // Only new closure is examined. Existing cursor rows have already been
        // issued; future output custody cannot become a visible item dependency.
        let mut reachable_new = imbl::OrdSet::new();
        let mut pending_nodes = new_roots.iter().copied().collect::<Vec<_>>();
        while let Some(node) = pending_nodes.pop() {
            if cursor_nodes.contains(&node) || reachable_new.insert(node).is_some() {
                continue;
            }
            state.graph_visits.fetch_add(1, Ordering::Relaxed);
            if fixed.entries[&node.artifact()].descriptor.kind == ArtifactKind::ValueInterface
                && !allowed_values.contains(&node.artifact())
                && !cursor.contains_namespace_artifact(node.artifact())
            {
                return Err(failure("bound target reaches a future value interface"));
            }
            match bound.get(&node) {
                Some(edges) => pending_nodes.extend(edges.iter().map(|(target, _)| *target)),
                None => {
                    let index = state
                        .indices
                        .get(&node)
                        .ok_or_else(|| failure("bound target is outside issued custody"))?;
                    pending_nodes.extend(
                        state
                            .graph
                            .edges(*index)
                            .map(|edge| state.graph[edge.target()]),
                    );
                }
            }
        }
        let prepared_proofs = scope_proof::prepare_install(&state, witness)?;
        let commit = BindingCommit::prepare(&state, &fixed.entries, &bound)?;
        let roots = new_roots
            .into_iter()
            .filter(|node| !cursor_nodes.contains(node))
            .collect::<Vec<_>>();
        let mut issuing_payloads = BTreeMap::new();
        for id in artifact_ids(&reachable_new) {
            let incoming = &fixed.entries[&id];
            issuing_payloads.insert(id, Arc::clone(incoming));
        }
        let issued_count = bound
            .keys()
            .filter(|node| !state.indices.contains_key(node))
            .count();
        let reused_count = reused.len() + bound.len() - issued_count;
        let mut closed_nodes = cursor_projection.nodes.clone();
        closed_nodes.extend(reachable_new.iter().copied());
        let mut closed_entries = cursor_projection.entries.clone();
        let mut issuing_ids = cursor_projection.issuing_ids.clone();
        for (id, entry) in &issuing_payloads {
            match closed_entries.binary_search_by(|old| {
                (&old.descriptor.owner, old.descriptor.id).cmp(&(&entry.descriptor.owner, *id))
            }) {
                Ok(position) => {
                    closed_entries.set(position, Arc::clone(entry));
                }
                Err(position) => {
                    closed_entries.insert(position, Arc::clone(entry));
                }
            }
            state.entry_handle_copies.fetch_add(1, Ordering::Relaxed);
            issuing_ids.insert(*id);
        }
        let projection = ViewReadProjection {
            nodes: closed_nodes,
            entries: closed_entries,
            issuing_ids,
            dependencies: OnceLock::new(),
            native_selection: OnceLock::new(),
        };
        commit.commit(&mut state);
        scope_proof::install(&mut state, prepared_proofs);
        for root in &roots {
            *state.roots.entry(*root).or_default() += 1;
        }
        let parents = vec![Arc::clone(&cursor.lease)];
        let snapshot = selection_snapshot_for_lease(&state, &namespace, &roots, &[], &parents);
        let issued_groups = bound
            .keys()
            .filter_map(|node| match node {
                InventoryNodeKey::Group(group) => Some(*group),
                _ => None,
            })
            .collect::<Vec<_>>();
        // Cache newly issued availability with its own lease. It does not enter
        // the cursor's executable demand unless the caller accepts this result.
        for group in &issued_groups {
            *state
                .roots
                .entry(InventoryNodeKey::Group(*group))
                .or_default() += 1;
        }
        let hidden_roots = issued_groups
            .iter()
            .copied()
            .map(InventoryNodeKey::Group)
            .collect::<Vec<_>>();
        let hidden_parents = vec![Arc::clone(&fixed.view.lease)];
        let hidden_namespace = fixed.view.namespace();
        let hidden_snapshot = selection_snapshot_for_lease(
            &state,
            &hidden_namespace,
            &hidden_roots,
            &[],
            &hidden_parents,
        );
        let hidden_projection = if issued_groups.is_empty() {
            None
        } else {
            let previous = fixed.view.read_projection(&state);
            let mut nodes = previous.nodes.clone();
            // The committed group rows resolve every artifact and existing group
            // through fixed custody; their new group edges form the only delta.
            nodes.extend(bound.keys().copied());
            Some(ViewReadProjection {
                nodes,
                entries: previous.entries.clone(),
                issuing_ids: previous.issuing_ids.clone(),
                dependencies: OnceLock::new(),
                native_selection: OnceLock::new(),
            })
        };
        let totals = (
            state.canonical_hits.load(Ordering::Relaxed),
            state.canonical_comparisons.load(Ordering::Relaxed),
            state.canonical_bytes_offered.load(Ordering::Relaxed),
            state.installed_binding_rows.load(Ordering::Relaxed),
            state.graph_visits.load(Ordering::Relaxed),
            state.entry_handle_copies.load(Ordering::Relaxed),
        );
        drop(state);
        if !issued_groups.is_empty() {
            fixed.view = ArtifactView::new(ViewLease {
                selection: hidden_snapshot,
                inventory: self.clone(),
                roots: hidden_roots,
                custody_roots: vec![],
                parents: hidden_parents,
                issuing_payloads: BTreeMap::new(),
                materialization_parents: vec![],
                materialization: Mutex::new(BTreeMap::new()),
            });
            fixed
                .view
                .reads
                .read_projection
                .set(hidden_projection.expect("issued availability projection"))
                .ok()
                .expect("fresh availability projection");
            for group in &issued_groups {
                fixed.groups.insert(
                    NativeGroupKey {
                        artifact: group.binding.artifact,
                        original_ordinal: group.original_ordinal,
                    },
                    BTreeSet::from([*group]),
                );
            }
        }
        tracing::debug!(target: "exomonad_harness::timing", phase = "products.bound_native_demand", issued_native_groups = issued_count, reused_native_groups = reused_count, canonical_hits_total = totals.0, canonical_comparisons_total = totals.1, canonical_bytes_offered_total = totals.2, installed_binding_rows_total = totals.3, graph_visits_total = totals.4, entry_handle_copies_total = totals.5, "bound native demand inventory observation");
        let inherited_materializations = materialization_parents.clone();
        let view = ArtifactView::new(ViewLease {
            selection: snapshot,
            inventory: self.clone(),
            roots,
            custody_roots: vec![],
            parents,
            issuing_payloads,
            materialization_parents,
            materialization: Mutex::new(BTreeMap::new()),
        });
        view.reads
            .read_projection
            .set(projection)
            .ok()
            .expect("fresh bound projection");
        let _ = view
            .reads
            .inherited_materializations
            .set(inherited_materializations);
        Ok(view)
    }

    /// The compiler-role owner supplies the exact namespace before adding
    /// authenticated interface facts. Historical parents remain independent custody.
    pub(crate) fn admit_projected_with_native_requirements(
        &self,
        parent: &ArtifactView,
        entries: Vec<Arc<ArtifactEntry>>,
        projection: &CompilerInputProjection,
        requirements: &[NativeOriginalRequirement],
    ) -> Result<ArtifactView, CompileError> {
        let offered = projection
            .roles()
            .iter()
            .filter_map(CompilerInputRole::original)
            .collect::<BTreeSet<_>>();
        if let Some(requirement) = requirements
            .iter()
            .find(|fact| !offered.contains(&fact.original))
        {
            return Err(admission_failure(
                ArtifactInventoryFailure::NativeRootOutsideView {
                    artifact: requirement.original,
                },
            ));
        }
        self.admit_shared_with_intent(
            parent,
            entries,
            NativeArtifactDemand::ScopeInterfaces,
            ArtifactRootIntent::Projected(projection, requirements),
        )
    }
    fn admit_shared_with_intent(
        &self,
        parent: &ArtifactView,
        entries: Vec<Arc<ArtifactEntry>>,
        demand: NativeArtifactDemand<'_>,
        intent: ArtifactRootIntent<'_>,
    ) -> Result<ArtifactView, CompileError> {
        let groups = match demand {
            NativeArtifactDemand::ScopeInterfaces => BTreeSet::new(),
            NativeArtifactDemand::AllGroups => entries
                .iter()
                .flat_map(|entry| {
                    entry
                        .native_group_ordinals
                        .iter()
                        .map(move |ordinal| NativeGroupKey {
                            artifact: entry.descriptor.id,
                            original_ordinal: *ordinal,
                        })
                })
                .collect(),
            NativeArtifactDemand::VerifiedTarget { entry, imports } => {
                let mut groups = certified_target_source_groups(&entries, imports)?;
                groups.insert(entry.native_group_key());
                groups
            }
            NativeArtifactDemand::CertifiedTargetImports(imports) => {
                let inherited = parent.artifact_ids().into_iter().collect::<BTreeSet<_>>();
                let mut groups = entries
                    .iter()
                    .filter(|entry| !inherited.contains(&entry.descriptor.id))
                    .flat_map(|entry| {
                        entry
                            .native_group_ordinals
                            .iter()
                            .map(move |ordinal| NativeGroupKey {
                                artifact: entry.descriptor.id,
                                original_ordinal: *ordinal,
                            })
                    })
                    .collect::<BTreeSet<_>>();
                groups.extend(certified_target_source_groups(&entries, imports)?);
                groups
            }
        };
        self.admit_selected(parent, entries, groups, false, intent)
    }
    /// Persisted keys are checked against full certified bytes and exact closure.
    pub(crate) fn admit_recovery_selection(
        &self,
        parent: &ArtifactView,
        entries: Vec<Arc<ArtifactEntry>>,
        selected_native_groups: &BTreeSet<NativeGroupKey>,
    ) -> Result<ArtifactView, CompileError> {
        self.admit_selected(
            parent,
            entries,
            selected_native_groups.clone(),
            true,
            ArtifactRootIntent::SuppliedArtifacts,
        )
    }
    fn admit_selected(
        &self,
        parent: &ArtifactView,
        entries: Vec<Arc<ArtifactEntry>>,
        groups: BTreeSet<NativeGroupKey>,
        exact: bool,
        root_intent: ArtifactRootIntent<'_>,
    ) -> Result<ArtifactView, CompileError> {
        if !Arc::ptr_eq(&self.0, &parent.lease.inventory.0) {
            return Err(failure("view belongs to another inventory"));
        }
        if entries.is_empty()
            && groups.is_empty()
            && matches!(root_intent, ArtifactRootIntent::SuppliedArtifacts)
        {
            return Ok(parent.clone());
        }
        let materialization_parents = Vec::new();
        let retained_roots: Option<Vec<InventoryNodeKey>> = None;
        let custody_roots = Vec::<ArtifactBindingNode>::new();
        let mut expanded = entries;
        let mut implicit = Vec::new();
        for entry in &expanded {
            if let ArtifactPayload::Original(product) = &entry.payload {
                let canonical = product
                    .module_interface()
                    .ok_or_else(|| failure("native canonical carrier missing"))?;
                // An explicitly supplied canonical carrier can replace this
                // implicit carrier only when it also preserves its custody.
                if !expanded.iter().any(|available| {
                    let ArtifactPayload::Canonical(existing) = &available.payload else {
                        return false;
                    };
                    existing == canonical
                        && match canonical.catalog_input_custody() {
                            None => true,
                            Some(incoming) => existing
                                .catalog_input_custody()
                                .is_some_and(|retained| Arc::ptr_eq(&retained, &incoming)),
                        }
                }) {
                    implicit.push(Arc::new(ArtifactEntry::canonical(canonical.clone())));
                }
            }
        }
        expanded.extend(implicit);
        let mut state = self.0.lock().expect("inventory lock");
        let parent_projection = parent.read_projection(&state);
        let parent_nodes = &parent_projection.nodes;
        let parent_ids = artifact_ids(parent_nodes);
        let parent_entries = parent_projection
            .entries
            .iter()
            .map(|entry| (entry.descriptor.id, entry))
            .collect::<BTreeMap<_, _>>();
        let mut supplied = BTreeMap::new();
        for entry in expanded {
            let entry = state.canonicalize(entry)?;
            let id = entry.descriptor.id;
            let retained = if let Some(previous) = supplied
                .get(&id)
                .or_else(|| parent_entries.get(&id).copied())
            {
                ArtifactEntry::merge_catalog_input_custody(previous, &entry)?
            } else {
                // An unrelated global vertex supplies semantic bytes, never
                // another admission's source proof or physical custody.
                if state
                    .payloads
                    .get(&id)
                    .is_some_and(|existing| existing != &entry)
                {
                    return Err(admission_failure(
                        ArtifactInventoryFailure::MetadataConflict { artifact: id },
                    ));
                }
                entry
            };
            supplied.insert(id, retained);
        }
        // Resolve the incoming namespace from authenticated rows and the parent
        // selection. Reusing payload bytes never imports another view's edges.
        let mut selected = parent_entries
            .iter()
            .map(|(id, entry)| (*id, Arc::clone(entry)))
            .collect::<BTreeMap<_, _>>();
        selected.extend(supplied.clone());
        let namespace_ids = match root_intent {
            ArtifactRootIntent::Certified(issued) => Some(
                issued
                    .artifact_ids_from_entries(&selected)
                    .map_err(|error| CompileError::CompilerEvidence(Box::new(error)))?,
            ),
            ArtifactRootIntent::Projected(projection, _) => {
                projection.entries_from_artifacts(&selected)?;
                let ids = projection
                    .roles()
                    .into_iter()
                    .flat_map(|role| std::iter::once(role.interface()).chain(role.original()))
                    .collect::<BTreeSet<_>>();
                if ids.iter().any(|id| !selected.contains_key(id)) {
                    return Err(failure("projected namespace lacks authenticated artifact"));
                }
                Some(ids)
            }
            ArtifactRootIntent::SuppliedArtifacts => None,
        };
        let requirements = root_intent.native_original_requirements();
        let mut required_groups =
            BTreeMap::<NativeGroupKey, imbl::OrdSet<NativeGroupBinding>>::new();
        let mut required_roots =
            BTreeMap::<NativeGroupKey, imbl::OrdSet<NativeGroupBinding>>::new();
        for NativeOriginalRequirement {
            original,
            selection,
        } in &requirements
        {
            let graph = selection.graph_selection();
            validate_issued_selection(&state, parent_nodes, graph)?;
            if !graph
                .bindings
                .iter()
                .any(|binding| binding.artifact == *original)
            {
                return Err(admission_failure(
                    ArtifactInventoryFailure::NativeRootOutsideView {
                        artifact: *original,
                    },
                ));
            }
            for group in &graph.native_groups {
                let key = NativeGroupKey {
                    artifact: group.binding.artifact,
                    original_ordinal: group.original_ordinal,
                };
                required_groups.entry(key).or_default().insert(*group);
            }
            for root in &graph.roots {
                let ArtifactBindingNode::Group(group) = root else {
                    continue;
                };
                if group.binding.artifact == *original {
                    required_roots
                        .entry(NativeGroupKey {
                            artifact: *original,
                            original_ordinal: group.original_ordinal,
                        })
                        .or_default()
                        .insert(*group);
                }
            }
        }
        let mut prebound = BTreeMap::<NativeGroupKey, NativeGroupBinding>::new();
        let mut historical_groups = BTreeSet::new();
        for (group, issued) in &required_groups {
            let choices = required_roots.get(group).unwrap_or(issued);
            if choices.len() != 1 {
                if !required_roots.contains_key(group) && !groups.contains(group) {
                    historical_groups.insert(*group);
                    continue;
                }
                return Err(admission_failure(
                    ArtifactInventoryFailure::BindingAmbiguity {
                        artifact: group.artifact,
                    },
                ));
            }
            prebound.insert(*group, *choices.iter().next().unwrap());
        }
        // Traverse the union once. Exact handles distinguish independently
        // issued children with the same payload; the demanding group gives
        // deterministic refusal evidence for every retained row.
        let mut required_nodes = BTreeMap::new();
        let mut pending = prebound
            .iter()
            .map(|(group, binding)| (InventoryNodeKey::Group(*binding), *group))
            .collect::<Vec<_>>();
        let mut cursor = 0;
        while let Some((node, demanding)) = pending.get(cursor).copied() {
            cursor += 1;
            if required_nodes.contains_key(&node) {
                continue;
            }
            required_nodes.insert(node, demanding);
            state.graph_visits.fetch_add(1, Ordering::Relaxed);
            let children = state
                .graph
                .edges(state.indices[&node])
                .map(|edge| state.graph[edge.target()])
                .collect::<BTreeSet<_>>();
            pending.extend(children.into_iter().map(|node| (node, demanding)));
        }
        let required_artifact_ids = required_nodes
            .keys()
            .filter_map(|node| match node {
                InventoryNodeKey::Artifact(binding) => Some(binding.artifact),
                InventoryNodeKey::Group(_) => None,
            })
            .collect::<BTreeSet<_>>();
        if let Some(ids) = &namespace_ids {
            let interfaces = ids
                .iter()
                .filter_map(|id| {
                    let entry = &selected[id];
                    (!entry.is_native()).then_some((entry.descriptor.owner.clone(), *id))
                })
                .collect::<BTreeMap<_, _>>();
            selected.retain(|id, entry| {
                ids.contains(id)
                    || (required_artifact_ids.contains(id)
                        && (entry.is_native()
                            || interfaces
                                .get(&entry.descriptor.owner)
                                .is_none_or(|chosen| chosen == id)))
                    || match &entry.payload {
                        ArtifactPayload::Original(product) => {
                            !parent_ids.contains(id)
                                && product.module_interface().is_some_and(|canonical| {
                                    ids.contains(
                                        &ArtifactEntry::canonical(canonical.clone()).descriptor.id,
                                    )
                                })
                        }
                        _ => false,
                    }
            });
            // Historical originals keep their issuer's exact bindings. An
            // unowned historical product cannot borrow an ambient graph path.
            for (id, entry) in &supplied {
                if matches!(entry.payload, ArtifactPayload::Original(_))
                    && !selected.contains_key(id)
                    && !parent_ids.contains(id)
                {
                    return Err(failure(
                        "historical original lacks its issuing bound closure",
                    ));
                }
            }
        }
        let owners = SelectedOwners::new(&state, &selected, &parent_ids)?;
        let mut selected_groups = native_groups(parent_nodes);
        selected_groups.extend(groups.iter().copied());
        if let Some(ids) = &namespace_ids {
            selected_groups.retain(|group| {
                selected.contains_key(&group.artifact)
                    && (ids.contains(&group.artifact)
                        || groups.contains(group)
                        || required_groups.contains_key(group))
            });
        }
        let (mut planned, closed_groups) =
            owners.planned_edges(&state, &selected, &selected_groups)?;
        if exact && closed_groups != selected_groups {
            return Err(failure(
                "recovered native group closure differs from certified demand",
            ));
        }
        // Exact binding differences cannot change certified dependency
        // payloads. Check every closure row, including transitive value bindings,
        // before selecting compiler namespace bindings independently.
        for (node, demanding) in &required_nodes {
            let actual = state
                .graph
                .edges(state.indices[node])
                .map(|edge| (state.graph[edge.target()].logical(), edge.weight().clone()))
                .collect::<BTreeSet<_>>();
            if planned
                .get(&node.logical())
                .is_none_or(|expected| expected != &actual)
            {
                return Err(admission_failure(
                    ArtifactInventoryFailure::NativeGroupSelectionConflict {
                        artifact: demanding.artifact,
                        original_ordinal: demanding.original_ordinal,
                    },
                ));
            }
        }
        let prebound_node = |key: &LogicalNodeKey| match key {
            LogicalNodeKey::Group(group) => {
                prebound.get(group).copied().map(InventoryNodeKey::Group)
            }
            LogicalNodeKey::Artifact(_) => None,
        };
        // Historical descendants retain their issuer's exact edges. Only a
        // selected root can qualify one of several handles for new execution.
        for (key, edges) in &planned {
            if prebound_node(key).is_some()
                || matches!(key, LogicalNodeKey::Group(group) if historical_groups.contains(group))
            {
                continue;
            }
            for (target, _) in edges {
                if let LogicalNodeKey::Group(group) = target {
                    if historical_groups.contains(group) {
                        return Err(admission_failure(
                            ArtifactInventoryFailure::BindingAmbiguity {
                                artifact: group.artifact,
                            },
                        ));
                    }
                }
            }
        }
        planned.retain(|key, _| {
            !matches!(key, LogicalNodeKey::Group(group) if historical_groups.contains(group))
        });
        // Reuse inherited bindings only while their entire qualified outgoing
        // edge set matches the newly issued namespace. Changes propagate through
        // parents; unrelated inherited vertices are never rebound or orphaned.
        let provisional = ArtifactSelectionId([0; 32]);
        let namespace = parent.namespace();
        // Compiler roles can choose artifact bindings. Group issuance is
        // independent: compare exact handles and their complete labeled rows,
        // including the recorded carrier, rather than carrier bytes/ordinal.
        let mut candidates = planned
            .keys()
            .filter(|key| prebound_node(key).is_none())
            .map(|key| (*key, BTreeSet::new()))
            .collect::<BTreeMap<_, _>>();
        for candidate in parent_nodes {
            let key = candidate.logical();
            let Some(matching) = candidates.get_mut(&key) else {
                continue;
            };
            if let InventoryNodeKey::Artifact(binding) = candidate {
                if namespace
                    .get(&binding.artifact)
                    .is_some_and(|selected| selected != binding)
                {
                    continue;
                }
            }
            let actual = state
                .graph
                .edges(state.indices[candidate])
                .map(|edge| (state.graph[edge.target()].logical(), edge.weight().clone()))
                .collect::<BTreeSet<_>>();
            if actual == planned[&key] {
                matching.insert(*candidate);
            }
        }
        loop {
            let mut invalid = Vec::new();
            for (key, matching) in &candidates {
                for candidate in matching {
                    if state.graph.edges(state.indices[candidate]).any(|edge| {
                        let target = state.graph[edge.target()];
                        match prebound_node(&target.logical()) {
                            Some(selected) => selected != target,
                            None => !candidates
                                .get(&target.logical())
                                .is_some_and(|matching| matching.contains(&target)),
                        }
                    }) {
                        invalid.push((*key, *candidate));
                    }
                }
            }
            if invalid.is_empty() {
                // Several eligible historical handles cannot identify this
                // request's issuer. Issue the row in the new scope, then
                // propagate its new exact handle through dependent rows.
                invalid.extend(
                    candidates
                        .iter()
                        .filter(|(_, matching)| matching.len() > 1)
                        .flat_map(|(key, matching)| {
                            matching.iter().map(|candidate| (*key, *candidate))
                        }),
                );
            }
            if invalid.is_empty() {
                break;
            }
            for (key, candidate) in invalid {
                candidates.get_mut(&key).unwrap().remove(&candidate);
            }
        }
        let inherited = candidates
            .into_iter()
            .filter(|(_, matching)| !matching.is_empty())
            .map(|(key, matching)| (key, *matching.first().unwrap()))
            .collect::<BTreeMap<_, _>>();
        let provisional_plan = planned
            .iter()
            .filter(|(key, _)| prebound_node(key).is_none())
            .map(|(key, edges)| {
                (
                    inherited
                        .get(key)
                        .copied()
                        .unwrap_or_else(|| key.bind(provisional)),
                    edges
                        .iter()
                        .map(|(target, edge)| {
                            (
                                inherited
                                    .get(target)
                                    .copied()
                                    .or_else(|| prebound_node(target))
                                    .unwrap_or_else(|| target.bind(provisional)),
                                edge.clone(),
                            )
                        })
                        .collect::<BTreeSet<_>>(),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let witness = ScopeProofV3::issue(&provisional_plan, provisional).map(Arc::new);
        let selection = witness.as_ref().map(|proof| proof.selection);
        let resolve = |key: LogicalNodeKey| {
            prebound_node(&key)
                .or_else(|| inherited.get(&key).copied())
                .unwrap_or_else(|| key.bind(selection.expect("new node has an issued scope")))
        };
        let bound = planned
            .iter()
            .filter(|(key, _)| prebound_node(key).is_none())
            .map(|(key, edges)| {
                (
                    resolve(*key),
                    edges
                        .iter()
                        .map(|(target, edge)| (resolve(*target), edge.clone()))
                        .collect::<BTreeSet<_>>(),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let namespace = selected
            .keys()
            .filter(|id| namespace_ids.as_ref().is_none_or(|ids| ids.contains(id)))
            .map(|id| {
                let InventoryNodeKey::Artifact(binding) = resolve(LogicalNodeKey::Artifact(*id))
                else {
                    unreachable!("artifact plan resolves an artifact node")
                };
                (*id, binding)
            })
            .collect::<BTreeMap<_, _>>();
        let roots = retained_roots.unwrap_or_else(|| {
            supplied
                .keys()
                .filter(|id| selected.contains_key(id))
                .copied()
                .map(LogicalNodeKey::Artifact)
                .chain(
                    groups
                        .iter()
                        .filter(|group| selected.contains_key(&group.artifact))
                        .copied()
                        .map(LogicalNodeKey::Group),
                )
                .chain(
                    parent
                        .roots()
                        .iter()
                        .map(|key| key.logical())
                        .filter(|key| planned.contains_key(key)),
                )
                .map(resolve)
                .filter(|node| {
                    matches!(node, InventoryNodeKey::Group(_))
                        || parent.roots().binary_search(node).is_err()
                })
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect()
        });
        // The prior selected closure stays immutable and independently owned.
        // Incoming shared carriers use the newly issued namespace binding.
        let issuing_payloads = selected
            .iter()
            .filter(|(id, entry)| match parent_entries.get(id) {
                Some(previous) => !previous.same_issuing_authority(entry),
                None => {
                    entry.catalog_input_custody().is_some()
                        || entry.source_admission_witness().is_some()
                }
            })
            .map(|(id, entry)| (*id, Arc::clone(entry)))
            .collect();
        // Validate proof retention before installing graph vertices. No payload
        // is retained for an unselected source row in an ancestor proof.
        let prepared_proofs = scope_proof::prepare_install(&state, witness)?;
        BindingCommit::prepare(&state, &selected, &bound)?.commit(&mut state);
        scope_proof::install(&mut state, prepared_proofs);
        for key in roots.iter().copied().chain(custody_roots.iter().copied()) {
            *state.roots.entry(key).or_default() += 1;
        }
        let parents = vec![Arc::clone(&parent.lease)];
        let snapshot =
            selection_snapshot_for_lease(&state, &namespace, &roots, &custody_roots, &parents);
        drop(state);
        Ok(ArtifactView::new(ViewLease {
            selection: snapshot,
            inventory: self.clone(),
            roots,
            custody_roots,
            parents,
            issuing_payloads,
            materialization_parents,
            materialization: Mutex::new(BTreeMap::new()),
        }))
    }
    /// Restore exact bound facts only after validating every certified edge and
    /// the current namespace. Decoded selection IDs grant no compiler authority.
    pub(crate) fn recover_selection(
        &self,
        parent: &ArtifactView,
        entries: Vec<Arc<ArtifactEntry>>,
        selection: &ArtifactGraphSelection,
        edges: &[(ArtifactBindingNode, ArtifactBindingNode, ArtifactDependency)],
    ) -> Result<ArtifactView, CompileError> {
        if !Arc::ptr_eq(&self.0, &parent.lease.inventory.0) || !parent.is_empty() {
            return Err(failure("bound recovery requires an empty owning view"));
        }
        let entries = entries
            .into_iter()
            .map(|entry| (entry.descriptor.id, entry))
            .collect::<BTreeMap<_, _>>();
        let bindings = selection.bindings.iter().copied().collect::<BTreeSet<_>>();
        let groups = selection
            .native_groups
            .iter()
            .copied()
            .collect::<BTreeSet<_>>();
        if bindings.len() != selection.bindings.len()
            || groups.len() != selection.native_groups.len()
        {
            return Err(failure("duplicate graph binding facts"));
        }
        let mut plan = bindings
            .iter()
            .copied()
            .map(InventoryNodeKey::Artifact)
            .chain(groups.iter().copied().map(InventoryNodeKey::Group))
            .map(|key| (key, BTreeSet::new()))
            .collect::<BTreeMap<_, _>>();
        for binding in &bindings {
            if !entries.contains_key(&binding.artifact) {
                return Err(failure("bound recovery payload missing"));
            }
        }
        for group in &groups {
            if !entries.get(&group.binding.artifact).is_some_and(|entry| {
                entry
                    .native_group_ordinals
                    .contains(&group.original_ordinal)
            }) {
                return Err(failure("bound recovery group lacks certified carrier"));
            }
        }
        for (source, target, edge) in edges {
            if !plan.contains_key(target) {
                return Err(failure("bound recovery dependency missing"));
            }
            if !plan
                .get_mut(source)
                .ok_or_else(|| failure("bound recovery dependent missing"))?
                .insert((*target, edge.clone()))
            {
                return Err(failure("duplicate bound recovery edge"));
            }
        }
        let witnesses = scope_proof::authenticate(&selection.selections, &plan)?;
        let mut state = self.0.lock().expect("inventory lock");
        let entries = entries
            .into_iter()
            .map(|(id, entry)| Ok((id, state.canonicalize(entry)?)))
            .collect::<Result<BTreeMap<_, _>, CompileError>>()?;
        cold_validation::validate_selected_rows(&state, &entries, &plan)?;
        let namespace = selection
            .namespace
            .iter()
            .map(|binding| (binding.artifact, *binding))
            .collect::<BTreeMap<_, _>>();
        if namespace.len() != selection.namespace.len()
            || namespace
                .values()
                .any(|binding| !bindings.contains(binding))
        {
            return Err(failure("invalid bound recovery namespace"));
        }
        let namespace_entries = namespace
            .keys()
            .map(|id| (*id, Arc::clone(&entries[id])))
            .collect();
        SelectedOwners::new(&state, &namespace_entries, &BTreeSet::new())?;
        let roots = selection.roots.clone();
        if roots.iter().copied().collect::<BTreeSet<_>>().len() != roots.len()
            || selection
                .custody_roots
                .iter()
                .copied()
                .collect::<BTreeSet<_>>()
                .len()
                != selection.custody_roots.len()
        {
            return Err(failure("duplicate bound recovery roots"));
        }
        if roots.iter().any(|key| !plan.contains_key(key))
            || selection
                .custody_roots
                .iter()
                .any(|node| !plan.contains_key(node))
        {
            return Err(failure("bound recovery root outside exact graph"));
        }
        let mut reachable = BTreeSet::new();
        let mut pending = roots
            .iter()
            .copied()
            .chain(selection.custody_roots.iter().copied())
            .collect::<Vec<_>>();
        while let Some(key) = pending.pop() {
            if reachable.insert(key) {
                pending.extend(plan[&key].iter().map(|(target, _)| *target));
            }
        }
        if reachable.len() != plan.len() {
            return Err(failure("bound recovery includes unreachable bindings"));
        }
        let prepared_proofs =
            scope_proof::prepare_install(&state, witnesses.into_values().map(|(proof, _)| proof))?;
        BindingCommit::prepare(&state, &entries, &plan)?.commit(&mut state);
        scope_proof::install(&mut state, prepared_proofs);
        for key in roots
            .iter()
            .copied()
            .chain(selection.custody_roots.iter().copied())
        {
            *state.roots.entry(key).or_default() += 1;
        }
        let snapshot =
            selection_snapshot_for_lease(&state, &namespace, &roots, &selection.custody_roots, &[]);
        drop(state);
        Ok(ArtifactView::new(ViewLease {
            selection: snapshot,
            inventory: self.clone(),
            roots,
            custody_roots: selection.custody_roots.clone(),
            parents: vec![],
            issuing_payloads: entries,
            materialization_parents: vec![],
            materialization: Mutex::new(BTreeMap::new()),
        }))
    }

    pub fn metrics(&self) -> ArtifactInventoryMetrics {
        let state = self.0.lock().expect("inventory lock");
        ArtifactInventoryMetrics {
            nodes: state.graph.node_count(),
            graph_visits: state.graph_visits.load(Ordering::Relaxed),
            view_queries: state.view_queries.load(Ordering::Relaxed),
            entry_handle_copies: state.entry_handle_copies.load(Ordering::Relaxed),
            whole_graph_copies: 0,
            reclamation_runs: state.reclamation_runs,
            reclamation_candidate_nodes: state.reclamation_candidate_nodes,
            reclaimed_nodes: state.reclaimed_nodes,
            reclamation_elapsed_ns: state.reclamation_elapsed_ns,
            admission_owner_lookups: state.admission_owner_lookups.load(Ordering::Relaxed),
            canonical_hits: state.canonical_hits.load(Ordering::Relaxed),
            canonical_comparisons: state.canonical_comparisons.load(Ordering::Relaxed),
            canonical_bytes_offered: state.canonical_bytes_offered.load(Ordering::Relaxed),
            installed_binding_rows: state.installed_binding_rows.load(Ordering::Relaxed),
        }
    }
    pub fn node_count(&self) -> usize {
        self.0.lock().expect("inventory lock").graph.node_count()
    }
}

struct ViewLease {
    selection: Arc<ExactSelectionSnapshot>,
    inventory: ArtifactInventory,
    roots: Vec<InventoryNodeKey>,
    /// Exact dependency carriers needed by currently unselected bodies. These
    /// retain exact artifact/group closures without becoming source roots or
    /// executable group demand.
    custody_roots: Vec<ArtifactBindingNode>,
    parents: Vec<Arc<ViewLease>>,
    // Sparse immutable overrides for custody and fresh source admission proof.
    // Explicit None overrides mask a different parent's proof in a join.
    // Parent leases retain unchanged handles without copying the full catalog.
    issuing_payloads: BTreeMap<ArtifactId, Arc<ArtifactEntry>>,
    // Projected selection must retain issued files without importing its
    // source view's selection roots or executable authority.
    materialization_parents: Vec<Arc<crate::declaration_context::RetainedArtifactMaterialization>>,
    materialization:
        Mutex<BTreeMap<[u8; 32], Arc<crate::declaration_context::RetainedArtifactMaterialization>>>,
}

fn replace_snapshot_namespace(
    snapshot: &mut ExactSelectionSnapshot,
    namespace: &BTreeMap<ArtifactId, ArtifactBinding>,
) {
    let removed = snapshot
        .namespace
        .keys()
        .filter(|id| !namespace.contains_key(id))
        .copied()
        .collect::<Vec<_>>();
    for id in removed {
        snapshot.namespace.remove(&id);
    }
    for (id, binding) in namespace {
        if snapshot.namespace.get(id) != Some(binding) {
            snapshot.namespace.insert(*id, *binding);
        }
    }
}

/// Persistent facts are changed at their issuing operation. Native leases keep
/// their separate local root counts; snapshots share immutable selected facts.
fn selection_snapshot_for_lease(
    state: &InventoryState,
    namespace: &BTreeMap<ArtifactId, ArtifactBinding>,
    roots: &[InventoryNodeKey],
    custody: &[ArtifactBindingNode],
    parents: &[Arc<ViewLease>],
) -> Arc<ExactSelectionSnapshot> {
    let mut snapshot = parents
        .first()
        .map(|parent| parent.selection.as_ref().clone())
        .unwrap_or_default();
    for parent in parents.iter().skip(1) {
        for (scope, proof) in &parent.selection.scopes {
            if !snapshot.scopes.contains_key(scope) {
                snapshot.scopes.insert(*scope, proof.clone());
            }
        }
        for root in parent.selection.roots.keys() {
            if !snapshot.roots.contains_key(root) {
                snapshot.roots.insert(*root, ());
            }
        }
        for binding in parent.selection.custody_roots.keys() {
            if !snapshot.custody_roots.contains_key(binding) {
                snapshot.custody_roots.insert(*binding, ());
            }
        }
    }
    replace_snapshot_namespace(&mut snapshot, namespace);
    for root in roots {
        if !snapshot.roots.contains_key(root) {
            snapshot.roots.insert(*root, ());
        }
    }
    for binding in custody {
        if !snapshot.custody_roots.contains_key(binding) {
            snapshot.custody_roots.insert(*binding, ());
        }
    }
    let mut pending = roots
        .iter()
        .map(|root| root.selection())
        .chain(custody.iter().map(|node| node.selection()))
        .collect::<Vec<_>>();
    while let Some(scope) = pending.pop() {
        if snapshot.scopes.contains_key(&scope) {
            continue;
        }
        let proof = Arc::clone(&state.selection_plans[&scope]);
        pending.extend(proof.scope_dependencies());
        snapshot
            .scopes
            .insert(scope, ImmutableScopeProofRef::from_issued(proof));
    }
    Arc::new(snapshot)
}

/// Parent custody retains roots and materializations without retaining derived
/// read projections. Only explicitly held views keep these caches alive.
#[derive(Default)]
struct ViewReadCache {
    canonical_roots: OnceLock<Vec<InventoryNodeKey>>,
    canonical_custody_roots: OnceLock<Vec<ArtifactBindingNode>>,
    read_projection: OnceLock<ViewReadProjection>,
    selected_nodes: OnceLock<imbl::OrdSet<InventoryNodeKey>>,
    inherited_materializations:
        OnceLock<Vec<Arc<crate::declaration_context::RetainedArtifactMaterialization>>>,
    #[cfg(test)]
    root_parent_visits: AtomicU64,
}

/// Live vertices never change payload or outgoing edges: private installation
/// rejects conflicting incoming rows, and reclamation cannot remove a view's
/// rooted closure. This read projection therefore needs no invalidation. Stable
/// keys survive unrelated graph-slot reuse; entry handles copy no artifact bytes.
/// Neither projection nor detached metadata retains a view or request
/// materialization. Selected handles preserve their exact original custody.
struct ViewReadProjection {
    nodes: imbl::OrdSet<InventoryNodeKey>,
    entries: imbl::Vector<Arc<ArtifactEntry>>,
    issuing_ids: imbl::OrdSet<ArtifactId>,
    dependencies: OnceLock<Vec<(ArtifactId, ArtifactId, ArtifactDependency)>>,
    native_selection: OnceLock<NativeSelectionIndex>,
}

/// Exact group lookup within one immutable view. Ordinal availability is kept
/// separately because a group can exist without naming the selected carrier.
#[derive(Default)]
struct NativeSelectionIndex {
    ordinals: BTreeMap<ArtifactId, BTreeSet<u32>>,
    groups: BTreeMap<(ArtifactBinding, u32), NativeGroupResolution>,
}

#[derive(Clone, Copy)]
enum NativeGroupResolution {
    Unique(NativeGroupBinding),
    Ambiguous,
}

impl NativeSelectionIndex {
    fn build(state: &InventoryState, owned: &imbl::OrdSet<InventoryNodeKey>) -> Self {
        let mut index = Self::default();
        for key in owned {
            let InventoryNodeKey::Group(group) = key else {
                continue;
            };
            index
                .ordinals
                .entry(group.binding.artifact)
                .or_default()
                .insert(group.original_ordinal);
            for edge in state.graph.edges(state.indices[key]) {
                let InventoryNodeKey::Artifact(carrier) = state.graph[edge.target()] else {
                    continue;
                };
                if carrier.artifact != group.binding.artifact
                    || *edge.weight() != ArtifactDependency::Interface
                {
                    continue;
                }
                index.groups.entry((carrier, group.original_ordinal))
                    .and_modify(|resolution| {
                        if matches!(resolution, NativeGroupResolution::Unique(prior) if prior != group) {
                            *resolution = NativeGroupResolution::Ambiguous;
                        }
                    })
                    .or_insert(NativeGroupResolution::Unique(*group));
            }
        }
        index
    }

    fn resolve(
        &self,
        carrier: ArtifactBinding,
        ordinal: u32,
    ) -> Result<NativeGroupBinding, CompileError> {
        match self.groups.get(&(carrier, ordinal)) {
            Some(NativeGroupResolution::Unique(group)) => Ok(*group),
            Some(NativeGroupResolution::Ambiguous) => Err(admission_failure(
                ArtifactInventoryFailure::BindingAmbiguity {
                    artifact: carrier.artifact,
                },
            )),
            None => Err(admission_failure(
                ArtifactInventoryFailure::NativeGroupUnavailable {
                    artifact: carrier.artifact,
                    original_ordinal: ordinal,
                },
            )),
        }
    }
}

impl ViewReadProjection {
    fn native_selection(&self, state: &InventoryState) -> &NativeSelectionIndex {
        self.native_selection
            .get_or_init(|| NativeSelectionIndex::build(state, &self.nodes))
    }
}

impl ViewLease {
    fn lock_materialization(
        &self,
    ) -> std::io::Result<
        std::sync::MutexGuard<
            '_,
            BTreeMap<[u8; 32], Arc<crate::declaration_context::RetainedArtifactMaterialization>>,
        >,
    > {
        loop {
            crate::host_work::checkpoint()?;
            match self.materialization.try_lock() {
                Ok(retained) => return Ok(retained),
                Err(std::sync::TryLockError::WouldBlock) => {
                    std::thread::sleep(std::time::Duration::from_millis(2))
                }
                Err(std::sync::TryLockError::Poisoned(_)) => panic!("materialization lock"),
            }
        }
    }

    fn collect_materializations(
        self: &Arc<Self>,
        materializations: &mut Vec<
            Arc<crate::declaration_context::RetainedArtifactMaterialization>,
        >,
        visited: &mut BTreeSet<usize>,
    ) -> Result<(), CompileError> {
        let mut pending = vec![self];
        while let Some(view) = pending.pop() {
            if !visited.insert(Arc::as_ptr(view) as usize) {
                continue;
            }
            let retained = view.lock_materialization()?;
            if !retained.is_empty() {
                for materialization in retained.values() {
                    if !materializations
                        .iter()
                        .any(|existing| Arc::ptr_eq(existing, materialization))
                    {
                        materializations.push(Arc::clone(materialization));
                    }
                }
            } else {
                for materialization in &view.materialization_parents {
                    if !materializations
                        .iter()
                        .any(|existing| Arc::ptr_eq(existing, materialization))
                    {
                        materializations.push(Arc::clone(materialization));
                    }
                }
                pending.extend(view.parents.iter().rev());
            }
        }
        Ok(())
    }
}
impl Drop for ViewLease {
    fn drop(&mut self) {
        let mut state = self.inventory.0.lock().expect("inventory lock");
        let mut lost_roots = Vec::new();
        for id in self
            .roots
            .iter()
            .copied()
            .chain(self.custody_roots.iter().copied())
        {
            let count = state.roots.get_mut(&id).expect("retained root");
            *count -= 1;
            if *count == 0 {
                state.roots.remove(&id);
                lost_roots.push(id);
            }
        }
        if lost_roots.is_empty() {
            return;
        }
        // Only the lost roots' reachable closure can become unowned. Nodes
        // outside it remain retained, so their incoming edges seed survivors.
        // This also handles cycles without scanning unrelated graph history.
        let started = std::time::Instant::now();
        state.reclamation_runs += 1;
        let candidates = admitted_closure(&state, lost_roots.into_iter());
        state.reclamation_candidate_nodes += candidates.len() as u64;
        let survivors = candidates
            .iter()
            .copied()
            .filter(|id| {
                state.roots.contains_key(id)
                    || state
                        .graph
                        .edges_directed(state.indices[id], Direction::Incoming)
                        .any(|edge| !candidates.contains(&state.graph[edge.source()]))
            })
            .collect::<Vec<_>>();
        let retained = admitted_closure(&state, survivors.into_iter());
        for id in candidates.iter().filter(|id| !retained.contains(*id)) {
            let index = state.indices.remove(id).expect("indexed artifact");
            state.graph.remove_node(index);
            state.reclaimed_nodes += 1;
            if let InventoryNodeKey::Artifact(id) = id {
                let count = state
                    .payload_bindings
                    .get_mut(&id.artifact)
                    .expect("bound payload count");
                *count -= 1;
                if *count == 0 {
                    state.payload_bindings.remove(&id.artifact);
                    state.payloads.remove(&id.artifact).expect("owned payload");
                }
            }
        }
        state.reclamation_elapsed_ns += started.elapsed().as_nanos() as u64;
        // Parent drops after the lock guard, preserving recursive release.
    }
}
fn admitted_closure(
    state: &InventoryState,
    roots: impl Iterator<Item = InventoryNodeKey>,
) -> imbl::OrdSet<InventoryNodeKey> {
    let mut pending = roots.collect::<Vec<_>>();
    let mut seen = imbl::OrdSet::new();
    while let Some(id) = pending.pop() {
        // Planned seeds are validated by the admission solver, not represented
        // as retained vertices until its complete atomic plan is committed.
        let Some(index) = state.indices.get(&id) else {
            continue;
        };
        if seen.insert(id).is_none() {
            state.graph_visits.fetch_add(1, Ordering::Relaxed);
            pending.extend(
                state
                    .graph
                    .edges(*index)
                    .map(|edge| state.graph[edge.target()]),
            );
        }
    }
    seen
}

fn validate_issued_selection(
    state: &InventoryState,
    owned: &imbl::OrdSet<InventoryNodeKey>,
    graph: &ArtifactGraphSelection,
) -> Result<(), CompileError> {
    let expected = graph
        .bindings
        .iter()
        .copied()
        .map(InventoryNodeKey::Artifact)
        .chain(
            graph
                .native_groups
                .iter()
                .copied()
                .map(InventoryNodeKey::Group),
        )
        .collect::<imbl::OrdSet<_>>();
    let seeds = graph
        .roots
        .iter()
        .copied()
        .chain(graph.custody_roots.iter().copied());
    if expected.len() != graph.bindings.len() + graph.native_groups.len()
        || graph.roots.iter().copied().collect::<BTreeSet<_>>().len() != graph.roots.len()
        || graph
            .custody_roots
            .iter()
            .copied()
            .collect::<BTreeSet<_>>()
            .len()
            != graph.custody_roots.len()
        || graph
            .namespace
            .iter()
            .map(|binding| binding.artifact)
            .collect::<BTreeSet<_>>()
            .len()
            != graph.namespace.len()
        || seeds.clone().any(|root| !expected.contains(&root))
        || graph
            .namespace
            .iter()
            .any(|binding| !expected.contains(&InventoryNodeKey::Artifact(*binding)))
        || admitted_closure(state, seeds) != expected
        || !expected.is_subset(owned)
    {
        return Err(failure(
            "issued selection differs from exact retained bindings",
        ));
    }
    Ok(())
}
fn artifact_ids(nodes: &imbl::OrdSet<InventoryNodeKey>) -> BTreeSet<ArtifactId> {
    nodes
        .iter()
        .filter_map(|key| match key {
            InventoryNodeKey::Artifact(binding) => Some(binding.artifact),
            _ => None,
        })
        .collect()
}
fn native_groups(nodes: &imbl::OrdSet<InventoryNodeKey>) -> BTreeSet<NativeGroupKey> {
    nodes
        .iter()
        .filter_map(|key| match key {
            InventoryNodeKey::Group(key) => Some(NativeGroupKey {
                artifact: key.binding.artifact,
                original_ordinal: key.original_ordinal,
            }),
            _ => None,
        })
        .collect()
}
/// Select only an owned group whose exact carrier edge names this artifact
/// binding. The group and carrier have independent issuing scopes.
fn resolve_group_binding(
    state: &InventoryState,
    owned: &imbl::OrdSet<InventoryNodeKey>,
    carrier: ArtifactBinding,
    ordinal: u32,
) -> Result<NativeGroupBinding, CompileError> {
    let mut candidates = owned.iter().filter_map(|key| {
        let InventoryNodeKey::Group(group) = key else {
            return None;
        };
        if group.binding.artifact != carrier.artifact || group.original_ordinal != ordinal {
            return None;
        }
        state
            .graph
            .edges(state.indices[key])
            .any(|edge| {
                state.graph[edge.target()] == InventoryNodeKey::Artifact(carrier)
                    && *edge.weight() == ArtifactDependency::Interface
            })
            .then_some(*group)
    });
    let group = candidates.next().ok_or_else(|| {
        admission_failure(ArtifactInventoryFailure::NativeGroupUnavailable {
            artifact: carrier.artifact,
            original_ordinal: ordinal,
        })
    })?;
    if candidates.next().is_some() {
        return Err(admission_failure(
            ArtifactInventoryFailure::BindingAmbiguity {
                artifact: carrier.artifact,
            },
        ));
    }
    Ok(group)
}

fn projected_dependencies(
    state: &InventoryState,
    nodes: &imbl::OrdSet<InventoryNodeKey>,
) -> Vec<(ArtifactId, ArtifactId, ArtifactDependency)> {
    let mut dependencies = BTreeSet::new();
    for key in nodes {
        #[cfg(test)]
        state
            .dependency_graph_visits
            .fetch_add(1, Ordering::Relaxed);
        for edge in state.graph.edges(state.indices[key]) {
            let target = state.graph[edge.target()];
            // The group-to-carrier custody edge has no public dependency row.
            if matches!((key, target), (InventoryNodeKey::Group(group), InventoryNodeKey::Artifact(carrier))
                if carrier.artifact == group.binding.artifact)
                && matches!(edge.weight(), ArtifactDependency::Interface)
            {
                continue;
            }
            dependencies.insert((key.artifact(), target.artifact(), edge.weight().clone()));
        }
    }
    dependencies.into_iter().collect()
}
/// Clones share custody and the immutable read cache. Descendants retain only
/// parent custody, so replacing a view releases its unobserved ancestor caches.
#[derive(Clone)]
pub struct ArtifactView {
    lease: Arc<ViewLease>,
    reads: Arc<ViewReadCache>,
}

/// One retained closure observed under the inventory lock. Entries are ordered
/// by exact owner; dependency tuples keep their canonical wire order.
pub(crate) struct ArtifactMetadataSnapshot {
    pub entries: BTreeMap<ExactModuleIdentity, Arc<ArtifactEntry>>,
    pub artifacts: BTreeMap<ArtifactId, Arc<ArtifactEntry>>,
    pub ambiguous_native_owners: BTreeSet<ExactModuleIdentity>,
    dependencies: Vec<(ArtifactId, ArtifactId, ArtifactDependency)>,
    pub(crate) selected_native_groups: BTreeSet<NativeGroupKey>,
}

impl ArtifactMetadataSnapshot {
    pub(crate) fn materialization_key(&self) -> [u8; 32] {
        digest(
            &serde_json::to_vec(&(
                self.entries
                    .values()
                    .map(|entry| entry.descriptor.id)
                    .collect::<Vec<_>>(),
                &self.selected_native_groups,
            ))
            .expect("compiler projection materialization key"),
        )
    }
    pub(crate) fn validate_native_selection(&self) -> Result<(), CompileError> {
        if let Some(owner) = self.ambiguous_native_owners.first() {
            return Err(admission_failure(
                ArtifactInventoryFailure::NativeOwnerAmbiguity {
                    owner: owner.clone(),
                },
            ));
        }
        Ok(())
    }
    /// The semantic wire order is exact owner followed by immutable artifact ID.
    /// The payload registry itself remains keyed by content ID.
    pub(crate) fn descriptors(&self) -> Vec<&ArtifactDescriptor> {
        let mut descriptors = self
            .artifacts
            .values()
            .map(|entry| &entry.descriptor)
            .collect::<Vec<_>>();
        descriptors.sort_by_key(|descriptor| (&descriptor.owner, descriptor.id));
        descriptors
    }
    pub fn dependencies(&self) -> Vec<(ArtifactId, ArtifactId, ArtifactDependency)> {
        self.dependencies.clone()
    }
}
impl std::fmt::Debug for ArtifactView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ArtifactView")
            .field("roots", &self.lease.roots)
            .finish()
    }
}
impl ArtifactView {
    #[cfg(test)]
    pub(crate) fn retained_issuing_payload_handles(&self) -> usize {
        let mut pending = vec![&self.lease];
        let mut seen = BTreeSet::new();
        let mut handles = 0;
        while let Some(lease) = pending.pop() {
            if seen.insert(Arc::as_ptr(lease)) {
                handles += lease.issuing_payloads.len();
                pending.extend(lease.parents.iter());
            }
        }
        handles
    }

    /// Provenance follows the selected certified payload's issuing owner.
    pub(crate) fn original_input_origins(
        &self,
        id: ArtifactId,
    ) -> Vec<crate::declaration_context::original_inputs::OwnedOriginalInputOrigins> {
        let state = self.lease.inventory.0.lock().expect("inventory lock");
        self.read_projection(&state)
            .entries
            .iter()
            .filter(|entry| entry.descriptor.id == id)
            .filter_map(|entry| match &entry.payload {
                ArtifactPayload::Original(product) => product.module_interface(),
                ArtifactPayload::Canonical(interface) => Some(interface),
                ArtifactPayload::Interface(_, _) => None,
            })
            .filter_map(|interface| interface.original_input_origins().cloned())
            .collect()
    }

    fn new(lease: ViewLease) -> Self {
        Self {
            lease: Arc::new(lease),
            reads: Arc::new(ViewReadCache::default()),
        }
    }

    // Closure and dependency initializers receive an already locked inventory. Nothing
    // initialized under either OnceLock acquires that mutex, so concurrent first
    // reads and admission always take inventory -> projection in that order.
    fn selected_nodes(&self, state: &InventoryState) -> &imbl::OrdSet<InventoryNodeKey> {
        self.reads.selected_nodes.get_or_init(|| {
            if self.custody_roots().is_empty() {
                if let Some(projection) = self.reads.read_projection.get() {
                    return projection.nodes.clone();
                }
            }
            admitted_closure(state, self.roots().iter().copied())
        })
    }

    fn read_projection(&self, state: &InventoryState) -> &ViewReadProjection {
        self.reads.read_projection.get_or_init(|| {
            // Selected and retained closures coincide only without custody-only
            // roots. Reuse an already observed closure without promoting custody.
            let nodes = if self.custody_roots().is_empty() {
                self.reads.selected_nodes.get().cloned()
            } else {
                None
            }
            .unwrap_or_else(|| {
                admitted_closure(
                    state,
                    self.roots()
                        .iter()
                        .copied()
                        .chain(self.custody_roots().iter().copied()),
                )
            });
            let ids = artifact_ids(&nodes);
            let mut custody = BTreeMap::new();
            let mut pending = std::collections::VecDeque::from([&self.lease]);
            let mut seen = BTreeSet::new();
            while let Some(lease) = pending.pop_front() {
                if !seen.insert(Arc::as_ptr(lease)) {
                    continue;
                }
                for (id, entry) in &lease.issuing_payloads {
                    if ids.contains(id) {
                        custody.entry(*id).or_insert(entry);
                    }
                }
                pending.extend(lease.parents.iter());
            }
            let mut entries = ids
                .iter()
                .map(|id| Arc::clone(custody.get(id).copied().unwrap_or(&state.payloads[id])))
                .collect::<Vec<_>>();
            state
                .entry_handle_copies
                .fetch_add(entries.len() as u64, Ordering::Relaxed);
            entries.sort_by(|left, right| {
                (&left.descriptor.owner, left.descriptor.id)
                    .cmp(&(&right.descriptor.owner, right.descriptor.id))
            });
            ViewReadProjection {
                issuing_ids: custody.keys().copied().collect(),
                nodes,
                entries: entries.into_iter().collect(),
                dependencies: OnceLock::new(),
                native_selection: OnceLock::new(),
            }
        })
    }

    fn read_dependencies(
        &self,
        state: &InventoryState,
    ) -> &Vec<(ArtifactId, ArtifactId, ArtifactDependency)> {
        let projection = self.read_projection(state);
        projection
            .dependencies
            .get_or_init(|| projected_dependencies(state, &projection.nodes))
    }

    /// Private materialization belongs to this immutable graph view. Failed
    /// preparation leaves no retained entry; descendants borrow completed owners.
    pub(crate) fn retain_materialization(
        &self,
        metadata: &ArtifactMetadataSnapshot,
        prepare: impl FnOnce(
            Vec<Arc<crate::declaration_context::RetainedArtifactMaterialization>>,
        ) -> Result<
            crate::declaration_context::RetainedArtifactMaterialization,
            CompileError,
        >,
    ) -> Result<Arc<crate::declaration_context::RetainedArtifactMaterialization>, CompileError>
    {
        // Waiting for another producer must remain responsive to this scope's
        // stop edge without interrupting that producer or evicting its result.
        let mut retained = self.lease.lock_materialization()?;
        let key = metadata.materialization_key();
        if let Some(materialization) = retained.get(&key) {
            return Ok(Arc::clone(materialization));
        }
        let mut parents = self.lease.materialization_parents.clone();
        let mut visited = BTreeSet::new();
        for parent in &self.lease.parents {
            parent.collect_materializations(&mut parents, &mut visited)?;
        }
        let materialization = Arc::new(prepare(parents)?);
        retained.insert(key, Arc::clone(&materialization));
        Ok(materialization)
    }

    fn collect_cached_materializations(
        &self,
        materializations: &mut Vec<
            Arc<crate::declaration_context::RetainedArtifactMaterialization>,
        >,
    ) -> Result<(), CompileError> {
        if self.reads.inherited_materializations.get().is_none() {
            let mut inherited = self.lease.materialization_parents.clone();
            let mut visited = BTreeSet::new();
            for parent in &self.lease.parents {
                parent.collect_materializations(&mut inherited, &mut visited)?;
            }
            let _ = self.reads.inherited_materializations.set(inherited);
        }
        let own = self.lease.lock_materialization()?;
        for materialization in self
            .reads
            .inherited_materializations
            .get()
            .unwrap()
            .iter()
            .chain(own.values())
        {
            if !materializations
                .iter()
                .any(|old| Arc::ptr_eq(old, materialization))
            {
                materializations.push(Arc::clone(materialization));
            }
        }
        Ok(())
    }

    fn collect_materializations(
        &self,
        materializations: &mut Vec<
            Arc<crate::declaration_context::RetainedArtifactMaterialization>,
        >,
        visited: &mut BTreeSet<usize>,
    ) -> Result<(), CompileError> {
        self.lease
            .collect_materializations(materializations, visited)
    }

    pub(crate) fn retained_materialization(
        &self,
        metadata: &ArtifactMetadataSnapshot,
    ) -> Result<
        Option<Arc<crate::declaration_context::RetainedArtifactMaterialization>>,
        CompileError,
    > {
        Ok(self
            .lease
            .lock_materialization()?
            .get(&metadata.materialization_key())
            .cloned())
    }
    pub(crate) fn metadata_snapshot(&self) -> ArtifactMetadataSnapshot {
        let state = self.lease.inventory.0.lock().expect("inventory lock");
        state.view_queries.fetch_add(1, Ordering::Relaxed);
        let projection = self.read_projection(&state);
        let selected_native_groups = native_groups(self.selected_nodes(&state));
        let mut entries = BTreeMap::new();
        let mut artifacts = BTreeMap::new();
        let dependencies = self.read_dependencies(&state).clone();
        let mut natives = BTreeMap::<ExactModuleIdentity, Vec<ArtifactId>>::new();
        for entry in &projection.entries {
            let id = entry.descriptor.id;
            let owner = entry.descriptor.owner.clone();
            if entry.is_native() {
                natives.entry(owner).or_default().push(id);
            } else {
                entries.insert(owner, id);
            }
            artifacts.insert(id, Arc::clone(entry));
        }
        let mut ambiguous_native_owners = BTreeSet::new();
        for (owner, ids) in natives {
            if ids.len() == 1 {
                entries.insert(owner, ids[0]);
            } else {
                ambiguous_native_owners.insert(owner);
            }
        }
        let entries = entries
            .into_iter()
            .map(|(owner, id)| (owner, Arc::clone(&artifacts[&id])))
            .collect::<BTreeMap<_, _>>();
        state
            .entry_handle_copies
            .fetch_add((entries.len() + artifacts.len()) as u64, Ordering::Relaxed);
        ArtifactMetadataSnapshot {
            entries,
            artifacts,
            dependencies,
            ambiguous_native_owners,
            selected_native_groups,
        }
    }

    /// Observe issued roots independently of retained native byte custody.
    pub fn is_empty(&self) -> bool {
        self.lease.selection.roots.is_empty()
    }
    pub fn inventory(&self) -> &ArtifactInventory {
        &self.lease.inventory
    }
    pub fn artifact_ids(&self) -> Vec<ArtifactId> {
        self.entries().iter().map(|e| e.descriptor.id).collect()
    }
    pub fn descriptors(&self) -> Vec<ArtifactDescriptor> {
        self.entries()
            .iter()
            .map(|e| e.descriptor.clone())
            .collect()
    }

    /// Canonical interfaces authorize types alongside their implementations;
    /// they cannot replace or grant a selected source implementation role.
    /// Classify retained implementations for lexical traversal. Exact variants
    /// share their kind; this census never selects a compiler original offer.
    pub(crate) fn source_implementation_roles(
        &self,
    ) -> BTreeMap<ExactModuleIdentity, ArtifactKind> {
        self.descriptors()
            .into_iter()
            .filter(|descriptor| descriptor.kind != ArtifactKind::CanonicalModuleInterface)
            .map(|descriptor| (descriptor.owner, descriptor.kind))
            .collect()
    }
    pub fn capture_graph_selection(&self) -> ArtifactGraphSelection {
        let state = self.lease.inventory.0.lock().expect("inventory lock");
        let nodes = &self.read_projection(&state).nodes;
        ArtifactGraphSelection {
            selections: self
                .lease
                .selection
                .scopes
                .values()
                .map(|reference| Arc::clone(reference.proof()))
                .collect(),
            bindings: nodes
                .iter()
                .filter_map(|key| match key {
                    InventoryNodeKey::Artifact(binding) => Some(*binding),
                    _ => None,
                })
                .collect(),
            namespace: self
                .namespace()
                .values()
                .filter(|binding| nodes.contains(&InventoryNodeKey::Artifact(**binding)))
                .copied()
                .collect(),
            roots: self.lease.selection.roots.keys().copied().collect(),
            native_groups: nodes
                .iter()
                .filter_map(|key| match key {
                    InventoryNodeKey::Group(group) => Some(*group),
                    _ => None,
                })
                .collect(),
            custody_roots: self.lease.selection.custody_roots.keys().copied().collect(),
        }
    }
    pub fn binding_dependencies(
        &self,
    ) -> Vec<(ArtifactBindingNode, ArtifactBindingNode, ArtifactDependency)> {
        let state = self.lease.inventory.0.lock().expect("inventory lock");
        let nodes = &self.read_projection(&state).nodes;
        nodes
            .iter()
            .flat_map(|source| {
                state
                    .graph
                    .edges(state.indices[source])
                    .map(|edge| (*source, state.graph[edge.target()], edge.weight().clone()))
            })
            .collect()
    }

    pub fn dependencies(&self) -> Vec<(ArtifactId, ArtifactId, ArtifactDependency)> {
        let state = self.lease.inventory.0.lock().expect("inventory lock");
        self.read_dependencies(&state).clone()
    }
    pub fn selected_native_groups(&self) -> BTreeSet<NativeGroupKey> {
        let state = self.lease.inventory.0.lock().expect("inventory lock");
        native_groups(self.selected_nodes(&state))
    }
    /// Interface dependencies remain independent of executable group demand.
    pub fn interface_dependencies(&self) -> Vec<(ArtifactId, ArtifactId, ArtifactDependency)> {
        self.dependencies()
            .into_iter()
            .filter(|(_, _, dependency)| matches!(dependency, ArtifactDependency::Interface))
            .collect()
    }
    /// Select native implementation dependencies independently of interface
    /// visibility. Roots explicitly select all groups or one issued ordinal;
    /// native group edges then select only their exact required ordinals.
    pub fn native_binding_requirements_from_roots(
        &self,
        roots: &[NativeRequirementRoot],
    ) -> Result<Vec<NativeBindingRequirement>, CompileError> {
        Ok(self.native_requirements_from_roots(roots)?.bindings)
    }

    /// Read-only lifetime requirements for accepted authored owners. Retaining a
    /// full module also retains private helpers that may be called by a later
    /// exported entry; this query grants no executable group selection.
    pub(crate) fn authored_native_binding_custody_requirements(
        &self,
        accepted_owners: &BTreeSet<ExactModuleIdentity>,
    ) -> Result<Vec<NativeBindingRequirement>, CompileError> {
        let state = self.lease.inventory.0.lock().expect("inventory lock");
        let owned = &self.read_projection(&state).nodes;
        let ids = artifact_ids(owned);
        let entries = ids
            .iter()
            .map(|id| (*id, Arc::clone(&state.payloads[id])))
            .collect::<BTreeMap<_, _>>();
        state
            .entry_handle_copies
            .fetch_add(entries.len() as u64, Ordering::Relaxed);
        let owners = SelectedOwners::new(&state, &entries, &ids)?;
        let roots = entries
            .values()
            .filter(|entry| {
                accepted_owners.contains(&entry.descriptor.owner)
                    && matches!(&entry.payload, ArtifactPayload::Original(product)
                        if product.module_interface().is_some_and(|interface|
                            matches!(interface.origin(), crate::certified_products::CanonicalOrigin::NativeAuthoredDeclaration { .. })))
            })
            .flat_map(|entry| entry.native_group_ordinals.iter().map(|ordinal| NativeGroupKey {
                artifact: entry.descriptor.id,
                original_ordinal: *ordinal,
            }))
            .collect();
        // The same owner/version/ordinal resolver used for executable admission
        // validates full-body dependencies, without installing its planned edges.
        let (_, groups) = owners.planned_edges(&state, &entries, &roots)?;
        let mut requirements = BTreeSet::new();
        for group in groups {
            state.graph_visits.fetch_add(1, Ordering::Relaxed);
            for (owner, dependency) in
                entries[&group.artifact].native_requirements_for(group.original_ordinal)
            {
                #[cfg(test)]
                state
                    .native_requirement_rows_examined
                    .fetch_add(1, Ordering::Relaxed);
                if let ArtifactDependency::NativeBinding {
                    generation,
                    namespace,
                    occurrence,
                    record_parent,
                    ..
                } = dependency
                {
                    let id = owners.interfaces.get(owner).copied().ok_or_else(|| {
                        failure("validated native binding lacks its interface owner")
                    })?;
                    requirements.insert(NativeBindingRequirement {
                        artifact_id: id,
                        identity: tidepool_repr::execution_schema::SymbolIdentity {
                            unit: owner.unit.clone(),
                            module: owner.module.clone(),
                            namespace: namespace.clone(),
                            occurrence: occurrence.clone(),
                            record_parent: record_parent.clone(),
                        },
                        generation: *generation,
                    });
                }
            }
        }
        Ok(requirements.into_iter().collect())
    }

    pub fn native_requirements_from_roots(
        &self,
        roots: &[NativeRequirementRoot],
    ) -> Result<NativeRequirements, CompileError> {
        Ok(self.native_requirements_with_groups_from_roots(roots)?.0)
    }

    /// Seal one typed entry's executable sites independently of native custody
    /// already retained by the parent view. Target imports select the wrapper's
    /// own original dependencies in addition to its checked authored entry.
    pub(crate) fn native_sites_for_target(
        &self,
        entry: &crate::checked_cell::CheckedTypedEntry,
        imports: &[crate::certified_products::PendingImportOwner],
    ) -> Result<crate::checked_cell::SelectedNativeSites, CompileError> {
        let groups = self.native_groups_for_target(Some(entry), imports)?;
        let state = self.lease.inventory.0.lock().expect("inventory lock");
        let mut sites = crate::checked_cell::SelectedNativeSites::default();
        for key in groups {
            let ArtifactPayload::Original(product) = &state.payloads[&key.binding.artifact].payload
            else {
                return Err(failure("selected native group lacks original payload"));
            };
            let original = crate::certified_products::original_native_group_definitions(
                product,
                key.original_ordinal,
            )
            .ok_or_else(|| failure("selected native group lacks authenticated sites"))?;
            for site in original.sites() {
                sites.insert_issued(site.clone(), Arc::clone(original.types()))?;
            }
        }
        Ok(sites)
    }

    pub(crate) fn native_groups_for_target(
        &self,
        entry: Option<&crate::checked_cell::CheckedTypedEntry>,
        imports: &[crate::certified_products::PendingImportOwner],
    ) -> Result<BTreeSet<NativeGroupBinding>, CompileError> {
        let mut roots = certified_target_source_groups(&self.entries(), imports)?;
        if let Some(entry) = entry {
            roots.insert(entry.native_group_key());
        }
        let roots = roots
            .into_iter()
            .map(|key| NativeRequirementRoot::Group {
                artifact: key.artifact,
                original_ordinal: key.original_ordinal,
            })
            .collect::<Vec<_>>();
        Ok(self
            .native_requirements_with_group_bindings_from_roots(&roots)?
            .1)
    }

    fn native_requirements_with_groups_from_roots(
        &self,
        roots: &[NativeRequirementRoot],
    ) -> Result<(NativeRequirements, BTreeSet<NativeGroupKey>), CompileError> {
        let (requirements, groups) =
            self.native_requirements_with_group_bindings_from_roots(roots)?;
        Ok((
            requirements,
            groups
                .into_iter()
                .map(|group| NativeGroupKey {
                    artifact: group.binding.artifact,
                    original_ordinal: group.original_ordinal,
                })
                .collect(),
        ))
    }

    fn native_requirements_with_group_bindings_from_roots(
        &self,
        roots: &[NativeRequirementRoot],
    ) -> Result<(NativeRequirements, BTreeSet<NativeGroupBinding>), CompileError> {
        let state = self.lease.inventory.0.lock().expect("inventory lock");
        let owned = &self.read_projection(&state).nodes;
        let mut pending = Vec::new();
        for root in roots {
            let (id, ordinals) = match *root {
                NativeRequirementRoot::AllGroups(id) => {
                    if self.resolve_binding(id, owned).is_err() {
                        return Err(admission_failure(
                            ArtifactInventoryFailure::NativeRootOutsideView { artifact: id },
                        ));
                    }
                    if !state.payloads[&id].is_native() {
                        return Err(failure("native root lacks an original native carrier"));
                    }
                    (id, state.payloads[&id].native_group_ordinals.clone())
                }
                NativeRequirementRoot::Group {
                    artifact,
                    original_ordinal,
                } => (artifact, BTreeSet::from([original_ordinal])),
            };
            if self.resolve_binding(id, owned).is_err() {
                return Err(admission_failure(
                    ArtifactInventoryFailure::NativeRootOutsideView { artifact: id },
                ));
            }
            for original_ordinal in ordinals {
                pending.push(resolve_group_binding(
                    &state,
                    owned,
                    self.resolve_binding(id, owned)?,
                    original_ordinal,
                )?);
            }
        }
        let mut seen = BTreeSet::new();
        let mut requirements = BTreeSet::new();
        let mut packages = BTreeSet::new();
        while let Some(key) = pending.pop() {
            if !seen.insert(key) {
                continue;
            }
            state.graph_visits.fetch_add(1, Ordering::Relaxed);
            for package in &state.payloads[&key.binding.artifact].retained_packages {
                if key.original_ordinal == package.dependent_ordinal {
                    packages.insert(NativePackageRequirement {
                        artifact_id: key.binding.artifact,
                        identity: package.identity.clone(),
                        generation: package.generation,
                        interface_digest: package.interface_digest,
                    });
                }
            }
            for edge in state
                .graph
                .edges(state.indices[&InventoryNodeKey::Group(key)])
            {
                let target = state.graph[edge.target()];
                match edge.weight() {
                    ArtifactDependency::NativeGroup { .. } => {
                        let InventoryNodeKey::Group(required) = target else {
                            return Err(failure("native group edge lacks group target"));
                        };
                        if !owned.contains(&target) {
                            return Err(failure("native dependency outside selected view"));
                        }
                        pending.push(required);
                    }
                    ArtifactDependency::NativeBinding {
                        generation,
                        namespace,
                        occurrence,
                        record_parent,
                        ..
                    } => {
                        let descriptor = &state.payloads[&target.artifact()].descriptor;
                        requirements.insert(NativeBindingRequirement {
                            artifact_id: descriptor.id,
                            identity: tidepool_repr::execution_schema::SymbolIdentity {
                                unit: descriptor.owner.unit.clone(),
                                module: descriptor.owner.module.clone(),
                                namespace: namespace.clone(),
                                occurrence: occurrence.clone(),
                                record_parent: record_parent.clone(),
                            },
                            generation: *generation,
                        });
                    }
                    _ => {}
                }
            }
        }
        Ok((
            NativeRequirements {
                bindings: requirements.into_iter().collect(),
                packages: packages.into_iter().collect(),
            },
            seen,
        ))
    }

    pub(crate) fn contains_issued_native_group(&self, group: NativeGroupBinding) -> bool {
        let state = self.lease.inventory.0.lock().expect("inventory lock");
        self.read_projection(&state)
            .nodes
            .contains(&InventoryNodeKey::Group(group))
    }

    pub(crate) fn select_publication_roots(
        &self,
        original: ArtifactId,
        roots: Vec<ArtifactId>,
    ) -> Result<Self, CompileError> {
        let ids = roots.into_iter().collect::<BTreeSet<_>>();
        let namespace = self.namespace();
        let mut selected = ids
            .iter()
            .map(|id| {
                namespace
                    .get(id)
                    .copied()
                    .map(InventoryNodeKey::Artifact)
                    .ok_or_else(|| {
                        failure("published compiler root lacks issued namespace binding")
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        selected.extend(self.roots().iter().copied().filter(|node| {
            matches!(node,
            InventoryNodeKey::Group(group) if group.binding.artifact == original)
        }));
        let state = self.lease.inventory.0.lock().expect("inventory lock");
        let owned = &self.read_projection(&state).nodes;
        let initial = artifact_ids(&admitted_closure(&state, selected.iter().copied()));
        let native_owners = artifact_ids(owned)
            .into_iter()
            .filter_map(|id| {
                let ArtifactPayload::Original(product) = &state.payloads[&id].payload else {
                    return None;
                };
                Some((NativeOwnerKey::from_owner(product.owner()), id))
            })
            .collect::<BTreeMap<_, _>>();
        let mut retained_ids = initial.clone();
        let mut pending = initial.iter().copied().collect::<Vec<_>>();
        while let Some(id) = pending.pop() {
            for required in state.payloads[&id].native_owners.values() {
                if let Some(required_id) = native_owners.get(required).copied() {
                    if retained_ids.insert(required_id) {
                        pending.push(required_id);
                    }
                }
            }
        }
        let mut custody = retained_ids
            .difference(&initial)
            .map(|id| {
                self.resolve_binding(*id, owned)
                    .map(InventoryNodeKey::Artifact)
            })
            .collect::<Result<Vec<_>, _>>()?;
        custody.extend(self.roots().iter().copied().filter(|node| matches!(node,
            InventoryNodeKey::Group(group) if group.binding.artifact != original && ids.contains(&group.binding.artifact))));
        if !selected.iter().any(|node| matches!(node, InventoryNodeKey::Group(group) if group.binding.artifact == original)) {
            let carrier = namespace[&original];
            let ordinals = owned.iter().filter_map(|node| match node {
                InventoryNodeKey::Group(group) if group.binding.artifact == original => Some(group.original_ordinal),
                _ => None,
            }).collect::<BTreeSet<_>>();
            for ordinal in ordinals {
                custody.push(InventoryNodeKey::Group(resolve_group_binding(&state, owned, carrier, ordinal)?));
            }
        }

        drop(state);
        self.retain_projected_materializations(self.lease.inventory.retain(
            selected,
            custody,
            Vec::new(),
            Vec::new(),
        ))
    }

    pub(crate) fn contains_namespace_artifact(&self, id: ArtifactId) -> bool {
        self.lease.selection.namespace.contains_key(&id)
    }

    fn namespace(&self) -> BTreeMap<ArtifactId, ArtifactBinding> {
        self.lease
            .selection
            .namespace
            .iter()
            .map(|(id, binding)| (*id, *binding))
            .collect()
    }

    pub(crate) fn selection_snapshot(&self) -> &Arc<ExactSelectionSnapshot> {
        &self.lease.selection
    }

    /// Attach hydrated sharing only after inventory recovery has reissued the
    /// same exact facts. Native graph counts remain owned by a separate lease.
    pub(crate) fn bind_selection_snapshot_baseline(
        &mut self,
        snapshot: Arc<ExactSelectionSnapshot>,
    ) -> Result<(), CompileError> {
        let live = self.selection_snapshot();
        if Arc::ptr_eq(live, &snapshot) {
            return Ok(());
        }
        if live.roots != snapshot.roots
            || live.namespace != snapshot.namespace
            || live.custody_roots != snapshot.custody_roots
            || live.scopes.len() != snapshot.scopes.len()
            || live.scopes.iter().any(|(scope, proof)| {
                snapshot
                    .scopes
                    .get(scope)
                    .is_none_or(|provided| proof.proof().as_ref() != provided.proof().as_ref())
            })
        {
            return Err(failure(
                "hydrated selection snapshot differs from inventory-issued facts",
            ));
        }
        let retained = self.lease.inventory.retain(
            self.roots().to_vec(),
            self.custody_roots().to_vec(),
            Vec::new(),
            Vec::new(),
        );
        let mut retained = self.retain_projected_materializations(retained)?;
        Arc::get_mut(&mut retained.lease)
            .expect("new selection lease")
            .selection = snapshot;
        *self = retained;
        Ok(())
    }

    fn replace_namespace(&mut self, namespace: &BTreeMap<ArtifactId, ArtifactBinding>) {
        let lease = Arc::get_mut(&mut self.lease).expect("new selection lease");
        replace_snapshot_namespace(Arc::make_mut(&mut lease.selection), namespace);
    }
    fn resolve_binding(
        &self,
        id: ArtifactId,
        owned: &imbl::OrdSet<InventoryNodeKey>,
    ) -> Result<ArtifactBinding, CompileError> {
        if let Some(binding) = self.lease.selection.namespace.get(&id) {
            if owned.contains(&InventoryNodeKey::Artifact(*binding)) {
                return Ok(*binding);
            }
        }
        let mut candidates = owned.iter().filter_map(|key| match key {
            InventoryNodeKey::Artifact(binding) if binding.artifact == id => Some(*binding),
            _ => None,
        });
        let binding = candidates
            .next()
            .ok_or_else(|| failure("selected artifact is outside retained view"))?;
        if candidates.next().is_some() {
            return Err(admission_failure(
                ArtifactInventoryFailure::BindingAmbiguity { artifact: id },
            ));
        }
        Ok(binding)
    }
    fn roots(&self) -> &[InventoryNodeKey] {
        self.reads.canonical_roots.get_or_init(|| {
            let mut pending = vec![&self.lease];
            let mut roots = BTreeSet::new();
            let mut seen = BTreeSet::new();
            while let Some(lease) = pending.pop() {
                if seen.insert(Arc::as_ptr(lease)) {
                    #[cfg(test)]
                    self.reads
                        .root_parent_visits
                        .fetch_add(1, Ordering::Relaxed);
                    roots.extend(lease.roots.iter().copied());
                    pending.extend(lease.parents.iter());
                }
            }
            roots.into_iter().collect()
        })
    }
    fn custody_roots(&self) -> &[ArtifactBindingNode] {
        self.reads.canonical_custody_roots.get_or_init(|| {
            let mut pending = vec![&self.lease];
            let mut custody = BTreeSet::new();
            let mut seen = BTreeSet::new();
            while let Some(lease) = pending.pop() {
                if seen.insert(Arc::as_ptr(lease)) {
                    custody.extend(lease.custody_roots.iter().copied());
                    pending.extend(lease.parents.iter());
                }
            }
            custody.into_iter().collect()
        })
    }
    /// Retain exactly these reachable artifact roots, independently of the
    /// source view's lifetime. Hidden dependencies remain graph-owned.
    pub fn select_roots(&self, roots: Vec<ArtifactId>) -> Result<Self, CompileError> {
        let state = self.lease.inventory.0.lock().expect("inventory lock");
        let projection = self.read_projection(&state);
        let owned = &projection.nodes;
        let native_selection = projection.native_selection(&state);
        let ids = roots.iter().copied().collect::<BTreeSet<_>>();
        let mut selected = roots
            .into_iter()
            .map(|id| {
                self.resolve_binding(id, owned)
                    .map(InventoryNodeKey::Artifact)
            })
            .collect::<Result<Vec<_>, _>>()?;
        for id in ids {
            let carrier = self.resolve_binding(id, owned)?;
            let ordinals = native_selection.ordinals.get(&id).into_iter().flatten();
            for &ordinal in ordinals {
                match native_selection.resolve(carrier, ordinal) {
                    Ok(group) => selected.push(InventoryNodeKey::Group(group)),
                    Err(CompileError::ArtifactInventory(error))
                        if matches!(
                            error.failure,
                            ArtifactInventoryFailure::NativeGroupUnavailable { .. }
                        ) => {}
                    Err(error) => return Err(error),
                }
            }
        }
        // A complete carrier can expose another body on a later request. Keep
        // its already issued native dependencies, including exact historical
        // versions, without promoting their groups or compiler input roles.
        // Resolve only inside this view; ambient inventory payloads grant no
        // custody and an unavailable body remains unavailable.
        let native_owners = artifact_ids(owned)
            .into_iter()
            .filter_map(|id| {
                let ArtifactPayload::Original(product) = &state.payloads[&id].payload else {
                    return None;
                };
                Some((NativeOwnerKey::from_owner(product.owner()), id))
            })
            .collect::<BTreeMap<_, _>>();
        let initial = artifact_ids(&admitted_closure(&state, selected.iter().copied()));
        let mut retained_ids = initial.clone();
        let mut pending = initial.iter().copied().collect::<Vec<_>>();
        while let Some(id) = pending.pop() {
            for required in state.payloads[&id].native_owners.values() {
                let Some(required_id) = native_owners.get(required).copied() else {
                    continue;
                };
                if retained_ids.insert(required_id) {
                    pending.push(required_id);
                }
            }
        }
        let custody_roots = retained_ids
            .difference(&initial)
            .map(|id| {
                self.resolve_binding(*id, owned)
                    .map(InventoryNodeKey::Artifact)
            })
            .collect::<Result<Vec<_>, _>>()?;
        drop(state);
        let retained = self
            .lease
            .inventory
            .retain(selected, custody_roots, Vec::new(), Vec::new());
        self.retain_projected_materializations(retained)
    }

    /// Narrow compiler support to issued roles without changing native demand.
    /// Existing custody-only groups keep their exact edges and remain private.
    fn select_compiler_support(
        &self,
        projection: &CompilerInputProjection,
    ) -> Result<Self, CompileError> {
        projection.validate(self)?;
        let ids = projection
            .roles()
            .into_iter()
            .flat_map(|role| std::iter::once(role.interface()).chain(role.original()))
            .collect::<BTreeSet<_>>();
        let state = self.lease.inventory.0.lock().expect("inventory lock");
        let owned = &self.read_projection(&state).nodes;
        let namespace = ids
            .iter()
            .map(|id| {
                self.resolve_binding(*id, owned)
                    .map(|binding| (*id, binding))
            })
            .collect::<Result<BTreeMap<_, _>, _>>()?;
        let mut roots = namespace
            .values()
            .copied()
            .map(InventoryNodeKey::Artifact)
            .collect::<Vec<_>>();
        let issued = self.roots().iter().copied().collect::<BTreeSet<_>>();
        roots.extend(issued.iter().copied().filter(|node| {
            matches!(node, InventoryNodeKey::Group(group) if ids.contains(&group.binding.artifact))
        }));
        let custody = owned
            .iter()
            .copied()
            .filter(|node| {
                matches!(node, InventoryNodeKey::Group(group)
                if ids.contains(&group.binding.artifact) && !issued.contains(node))
            })
            .collect::<Vec<_>>();
        drop(state);
        let mut retained = self
            .lease
            .inventory
            .retain(roots, custody, Vec::new(), Vec::new());
        retained.replace_namespace(&namespace);
        let mut retained = self.retain_projected_materializations(retained)?;
        retained.replace_namespace(&namespace);
        Ok(retained)
    }

    /// Preserve exact support nodes even when they were dependencies of the
    /// generated root. Native byte custody stays separate from source roots.
    pub(crate) fn support_without_owner(
        &self,
        excluded: &ExactModuleIdentity,
    ) -> Result<Self, CompileError> {
        let state = self.lease.inventory.0.lock().expect("inventory lock");
        if !self
            .read_projection(&state)
            .nodes
            .iter()
            .any(|node| &state.payloads[&node.artifact()].descriptor.owner == excluded)
        {
            return Ok(self.clone());
        }
        let selected = admitted_closure(&state, self.roots().iter().copied());
        let issued_roots = self.roots().iter().copied().collect::<BTreeSet<_>>();
        let roots = selected
            .iter()
            .filter(|node| {
                &state.payloads[&node.artifact()].descriptor.owner != excluded
                    && (matches!(node, InventoryNodeKey::Artifact(_))
                        || issued_roots.contains(node))
            })
            .copied()
            .collect::<Vec<_>>();
        let custody = self
            .custody_roots()
            .iter()
            .copied()
            .chain(selected.iter().copied().filter(|node| {
                matches!(node, InventoryNodeKey::Group(_)) && !issued_roots.contains(node)
            }))
            .filter(|node| &state.payloads[&node.artifact()].descriptor.owner != excluded)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        if admitted_closure(&state, roots.iter().copied().chain(custody.iter().copied()))
            .iter()
            .any(|node| &state.payloads[&node.artifact()].descriptor.owner == excluded)
        {
            return Err(failure(
                "support exclusion is required by retained exact dependencies",
            ));
        }
        drop(state);
        self.retain_projected_materializations(self.lease.inventory.retain(
            roots,
            custody,
            Vec::new(),
            Vec::new(),
        ))
    }

    /// Consume this admission's namespace and exact group roots before joining
    /// broader historical custody. The admission owner chose these handles;
    /// carrier bytes and ordinal alone cannot recreate that choice.
    pub(crate) fn select_admission_roots(
        &self,
        roots: Vec<ArtifactId>,
    ) -> Result<Self, CompileError> {
        let ids = roots.into_iter().collect::<BTreeSet<_>>();
        let namespace = self.namespace();
        let mut selected = ids
            .iter()
            .map(|id| {
                namespace
                    .get(id)
                    .copied()
                    .map(InventoryNodeKey::Artifact)
                    .ok_or_else(|| failure("admitted root lacks issued namespace binding"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        selected.extend(self.lease.roots.iter().filter_map(|node| match node {
            InventoryNodeKey::Group(group) if ids.contains(&group.binding.artifact) => Some(*node),
            _ => None,
        }));
        let state = self.lease.inventory.0.lock().expect("inventory lock");
        let owned = &self.read_projection(&state).nodes;
        if selected.iter().any(|root| !owned.contains(root)) {
            return Err(failure("admitted root outside issued graph"));
        }
        for id in &ids {
            if state.payloads[id].is_native()
                && state.payloads[id]
                    .native_group_ordinals
                    .iter()
                    .any(|ordinal| {
                        !selected.iter().any(|node| {
                            matches!(node,
                        InventoryNodeKey::Group(group)
                        if group.binding.artifact == *id && group.original_ordinal == *ordinal)
                        })
                    })
            {
                return Err(failure("admitted original lacks its issued group census"));
            }
        }
        drop(state);
        self.retain_projected_materializations(self.lease.inventory.retain(
            selected,
            Vec::new(),
            Vec::new(),
            Vec::new(),
        ))
    }

    /// Reopen an issuer's immutable selection inside subsequently grown custody.
    /// Additional available artifacts and demanded groups cannot change it.
    pub(crate) fn select_issued(
        &self,
        roots: Vec<ArtifactId>,
        selection: &ExactArtifactSelection,
    ) -> Result<Self, CompileError> {
        let graph = selection.graph_selection();
        let ids = graph
            .bindings
            .iter()
            .map(|binding| binding.artifact)
            .collect::<BTreeSet<_>>();
        if roots.iter().any(|id| !ids.contains(id)) {
            return Err(failure("issued root outside retained selection"));
        }
        let state = self.lease.inventory.0.lock().expect("inventory lock");
        let owned = &self.read_projection(&state).nodes;
        let namespace = graph
            .namespace
            .iter()
            .map(|binding| (binding.artifact, *binding))
            .collect::<BTreeMap<_, _>>();
        if roots.iter().any(|id| !namespace.contains_key(id)) {
            return Err(failure("issued root lacks exact namespace binding"));
        }
        let selected = graph.roots.clone();
        let custody = graph.custody_roots.clone();
        validate_issued_selection(&state, owned, graph)?;
        drop(state);
        let mut retained = self
            .lease
            .inventory
            .retain(selected, custody, Vec::new(), Vec::new());
        retained.replace_namespace(&namespace);
        let mut retained = self.retain_projected_materializations(retained)?;
        retained.replace_namespace(&namespace);
        Ok(retained)
    }

    fn retain_projected_materializations(&self, mut retained: Self) -> Result<Self, CompileError> {
        {
            let state = self.lease.inventory.0.lock().expect("inventory lock");
            let nodes = admitted_closure(
                &state,
                retained
                    .roots()
                    .iter()
                    .copied()
                    .chain(retained.custody_roots().iter().copied()),
            );
            let selected = artifact_ids(&nodes);
            if retained.lease.selection.namespace.is_empty() {
                let namespace = self
                    .namespace()
                    .into_iter()
                    .filter(|(_, binding)| nodes.contains(&InventoryNodeKey::Artifact(*binding)))
                    .collect();
                retained.replace_namespace(&namespace);
            }
            Arc::get_mut(&mut retained.lease)
                .expect("new selection lease")
                .issuing_payloads = self
                .read_projection(&state)
                .entries
                .iter()
                .filter(|entry| {
                    selected.contains(&entry.descriptor.id)
                        && self
                            .read_projection(&state)
                            .issuing_ids
                            .contains(&entry.descriptor.id)
                })
                .map(|entry| (entry.descriptor.id, Arc::clone(entry)))
                .collect();
        }
        let mut materializations = Vec::new();
        if !retained.is_empty() {
            self.collect_materializations(&mut materializations, &mut BTreeSet::new())?;
            if !materializations.is_empty() {
                let custody =
                    crate::declaration_context::RetainedArtifactMaterialization::select_custody(
                        &materializations,
                        &retained.metadata_snapshot(),
                    );
                Arc::get_mut(&mut retained.lease)
                    .expect("new selection lease")
                    .materialization_parents = custody.into_iter().collect();
            }
        }
        Ok(retained)
    }
    /// Preserve every source-owned original binding and already selected group
    /// as byte/native custody, without resolving payload IDs into compiler roles.
    pub(crate) fn retain_original_custody(&self, selected: &Self) -> Result<Self, CompileError> {
        if !Arc::ptr_eq(&self.lease.inventory.0, &selected.lease.inventory.0) {
            return Err(failure(
                "original custody selection belongs to another inventory",
            ));
        }
        let state = self.lease.inventory.0.lock().expect("inventory lock");
        let owned = &self.read_projection(&state).nodes;
        let selected_nodes = &selected.read_projection(&state).nodes;
        if !selected_nodes.is_subset(owned) {
            return Err(failure("original custody selection exceeds retained graph"));
        }
        let roots = selected.roots().to_vec();
        let mut custody = selected
            .custody_roots()
            .iter()
            .copied()
            .collect::<BTreeSet<_>>();
        custody.extend(
            owned
                .iter()
                .filter(|node| {
                    matches!(
                        state.payloads[&node.artifact()].payload,
                        ArtifactPayload::Original(_)
                    )
                })
                .copied(),
        );
        let namespace = selected.namespace();
        drop(state);
        let mut retained = self.lease.inventory.retain(
            roots,
            custody.into_iter().collect(),
            Vec::new(),
            Vec::new(),
        );
        retained.replace_namespace(&namespace);
        let mut retained = self.retain_projected_materializations(retained)?;
        // An empty selected namespace is also an issued fact, never a request
        // to inherit the source's broader compiler roles.
        retained.replace_namespace(&namespace);
        Ok(retained)
    }

    /// Narrow the compiler namespace using already-issued roles while retaining
    /// every exact historical binding and its physical custody independently.
    pub(crate) fn with_compiler_projection(
        &self,
        projection: &CompilerInputProjection,
    ) -> Result<Self, CompileError> {
        let state = self.lease.inventory.0.lock().expect("inventory lock");
        let nodes = &self.read_projection(&state).nodes;
        let namespace = projection
            .roles()
            .into_iter()
            .flat_map(|role| std::iter::once(role.interface()).chain(role.original()))
            .map(|id| self.resolve_binding(id, nodes).map(|binding| (id, binding)))
            .collect::<Result<BTreeMap<_, _>, _>>()?;
        if namespace == self.namespace() {
            return Ok(self.clone());
        }
        let entries = namespace
            .keys()
            .map(|id| (*id, Arc::clone(&state.payloads[id])))
            .collect();
        SelectedOwners::new(&state, &entries, &BTreeSet::new())?;
        let parents = vec![Arc::clone(&self.lease)];
        let selection = selection_snapshot_for_lease(&state, &namespace, &[], &[], &parents);
        drop(state);
        Ok(ArtifactView::new(ViewLease {
            selection,
            inventory: self.lease.inventory.clone(),
            roots: vec![],
            custody_roots: vec![],
            parents,
            issuing_payloads: BTreeMap::new(),
            materialization_parents: vec![],
            materialization: Mutex::new(BTreeMap::new()),
        }))
    }

    /// Join compiler inputs using each surviving role's issuing view. Other
    /// exact roots retain custody without becoming competing source choices.
    pub(crate) fn merge_compiler_inputs(
        &self,
        projection: &CompilerInputProjection,
        other: &Self,
        incoming: &CompilerInputProjection,
    ) -> Result<(Self, CompilerInputProjection), CompileError> {
        projection.validate(self)?;
        incoming.validate(other)?;
        let selected = projection.merge(incoming)?;
        let mut namespace = BTreeMap::new();
        let mut groups = BTreeMap::<NativeGroupKey, NativeGroupBinding>::new();
        for (owner, role) in selected.snapshot_roles() {
            let source = if projection.snapshot_roles().get(owner) == Some(role) {
                self
            } else if incoming.snapshot_roles().get(owner) == Some(role) {
                other
            } else {
                return Err(failure("merged compiler role lost its issuer"));
            };
            {
                let state = source.lease.inventory.0.lock().expect("inventory lock");
                let owned = &source.read_projection(&state).nodes;
                for id in std::iter::once(role.interface()).chain(role.original()) {
                    namespace.insert(id, source.resolve_binding(id, owned)?);
                }
            }
            let Some(original) = role.original() else {
                continue;
            };
            let requirement = match NativeOriginalRequirement::from_role(role.clone()) {
                Some(issued) => Some(issued),
                None => NativeOriginalRequirement::from_selected_view(original, source)?,
            };
            for requirement in requirement {
                for root in &requirement.selection.graph_selection().roots {
                    let ArtifactBindingNode::Group(group) = root else {
                        continue;
                    };
                    if group.binding.artifact != original {
                        continue;
                    }
                    let key = NativeGroupKey {
                        artifact: original,
                        original_ordinal: group.original_ordinal,
                    };
                    if groups
                        .insert(key, *group)
                        .is_some_and(|previous| previous != *group)
                    {
                        return Err(admission_failure(
                            ArtifactInventoryFailure::BindingAmbiguity { artifact: original },
                        ));
                    }
                }
            }
        }
        let joined = self.merge(other)?;
        let roots = namespace
            .values()
            .copied()
            .map(InventoryNodeKey::Artifact)
            .chain(groups.values().copied().map(InventoryNodeKey::Group))
            .collect::<BTreeSet<_>>();
        let custody = joined
            .roots()
            .iter()
            .copied()
            .chain(joined.custody_roots().iter().copied())
            .filter(|node| !roots.contains(node))
            .collect::<BTreeSet<_>>();
        let retained = joined.inventory().retain(
            roots.into_iter().collect(),
            custody.into_iter().collect(),
            Vec::new(),
            Vec::new(),
        );
        let mut retained = joined.retain_projected_materializations(retained)?;
        retained.replace_namespace(&namespace);
        Ok((retained, selected))
    }

    pub(crate) fn merge_selected(
        &self,
        other: &Self,
        projection: &CompilerInputProjection,
    ) -> Result<Self, CompileError> {
        let other = if Arc::ptr_eq(&self.lease.inventory.0, &other.lease.inventory.0) {
            other.clone()
        } else {
            let imported = self.lease.inventory.recover_selection(
                &self.lease.inventory.empty_view(),
                other.entries(),
                &other.capture_graph_selection(),
                &other.binding_dependencies(),
            )?;
            other.retain_projected_materializations(imported)?
        };
        let state = self.lease.inventory.0.lock().expect("inventory lock");
        let self_nodes = &self.read_projection(&state).nodes;
        let other_nodes = &other.read_projection(&state).nodes;
        let mut namespace = BTreeMap::new();
        for role in projection.roles() {
            for id in std::iter::once(role.interface()).chain(role.original()) {
                let binding = if other.namespace().contains_key(&id) {
                    other.resolve_binding(id, other_nodes)?
                } else {
                    self.resolve_binding(id, self_nodes)?
                };
                namespace.insert(id, binding);
            }
        }
        let entries = namespace
            .keys()
            .map(|id| (*id, Arc::clone(&state.payloads[id])))
            .collect();
        SelectedOwners::new(&state, &entries, &BTreeSet::new())?;
        let parents = vec![Arc::clone(&self.lease), Arc::clone(&other.lease)];
        let selection = selection_snapshot_for_lease(&state, &namespace, &[], &[], &parents);
        drop(state);
        Ok(ArtifactView::new(ViewLease {
            selection,
            inventory: self.lease.inventory.clone(),
            roots: vec![],
            custody_roots: vec![],
            parents,
            issuing_payloads: BTreeMap::new(),
            materialization_parents: vec![],
            materialization: Mutex::new(BTreeMap::new()),
        }))
    }

    pub(crate) fn merge(&self, other: &Self) -> Result<Self, CompileError> {
        if Arc::ptr_eq(&self.lease, &other.lease) || other.is_empty() {
            return Ok(self.clone());
        }
        if self.is_empty() {
            return Ok(other.clone());
        }
        if Arc::ptr_eq(&self.lease.inventory.0, &other.lease.inventory.0) {
            let state = self.lease.inventory.0.lock().expect("inventory lock");
            let parent_nodes = &self.read_projection(&state).nodes;
            let parent_ids = artifact_ids(parent_nodes);
            let mut nodes = parent_nodes.clone();
            nodes.extend(other.read_projection(&state).nodes.iter().copied());
            let ids = artifact_ids(&nodes);
            let mut entries = self
                .read_projection(&state)
                .entries
                .iter()
                .map(|entry| (entry.descriptor.id, Arc::clone(entry)))
                .collect::<BTreeMap<_, _>>();
            let mut issuing_payloads = BTreeMap::new();
            for entry in &other.read_projection(&state).entries {
                let id = entry.descriptor.id;
                let merged = match entries.get(&id) {
                    Some(previous) => {
                        let merged = ArtifactEntry::merge_catalog_input_custody(previous, entry)?;
                        if !merged.same_issuing_authority(previous)
                            || !merged.same_issuing_authority(entry)
                        {
                            issuing_payloads.insert(id, Arc::clone(&merged));
                        }
                        merged
                    }
                    None => Arc::clone(entry),
                };
                entries.insert(id, merged);
            }
            state
                .entry_handle_copies
                .fetch_add(ids.len() as u64, Ordering::Relaxed);
            SelectedOwners::new(&state, &entries, &parent_ids)?.planned_edges(
                &state,
                &entries,
                &native_groups(&nodes),
            )?;
            let parents = vec![Arc::clone(&self.lease), Arc::clone(&other.lease)];
            let mut namespace = self.namespace();
            namespace.extend(other.namespace());
            let selection = selection_snapshot_for_lease(&state, &namespace, &[], &[], &parents);
            drop(state);
            Ok(ArtifactView::new(ViewLease {
                selection,
                inventory: self.lease.inventory.clone(),
                roots: Vec::new(),
                custody_roots: Vec::new(),
                parents,
                issuing_payloads,
                materialization_parents: Vec::new(),
                materialization: Mutex::new(BTreeMap::new()),
            }))
        } else {
            let imported = self.lease.inventory.recover_selection(
                &self.lease.inventory.empty_view(),
                other.entries(),
                &other.capture_graph_selection(),
                &other.binding_dependencies(),
            )?;
            let imported = other.retain_projected_materializations(imported)?;
            self.merge(&imported)
        }
    }
    /// Reuse only this view's exact completed native carrier. Demand and graph
    /// closure remain the admission owner's responsibility for each consumer.
    pub(crate) fn retained_original_entry_with_validation(
        &self,
        producer: [u8; 32],
        product: &CertifiedRecoveryProduct,
        validation: &mut crate::recovery_artifacts::PackageInterfaceValidation,
    ) -> Result<Option<Arc<ArtifactEntry>>, CompileError> {
        let Some(native) = product.original_native() else {
            return Ok(None);
        };
        if !native.matches_original(product) {
            return Ok(None);
        }
        let descriptor = ArtifactEntry::original_descriptor(producer, product);
        let existing = {
            let state = self.lease.inventory.0.lock().expect("inventory lock");
            state.view_queries.fetch_add(1, Ordering::Relaxed);
            let retained = self
                .read_projection(&state)
                .entries
                .iter()
                .find(|entry| entry.descriptor.id == descriptor.id)
                .cloned();
            if retained.is_some() {
                state.entry_handle_copies.fetch_add(1, Ordering::Relaxed);
            }
            retained
        };
        let Some(existing) = existing else {
            return Ok(None);
        };
        let ArtifactPayload::Original(previous) = &existing.payload else {
            return Err(admission_failure(
                ArtifactInventoryFailure::MetadataConflict {
                    artifact: descriptor.id,
                },
            ));
        };
        if existing.descriptor != descriptor || !previous.same_durable_artifact(product) {
            return Err(admission_failure(
                ArtifactInventoryFailure::MetadataConflict {
                    artifact: descriptor.id,
                },
            ));
        }
        // Inventory equality deliberately excludes the fresh source witness.
        // A different source admission follows ordinary construction instead.
        if previous.source_sha256() != product.source_sha256() {
            return Ok(None);
        }
        let Some(previous_native) = previous.original_native() else {
            return Ok(None);
        };
        if !Arc::ptr_eq(native, previous_native) || !previous_native.matches_original(previous) {
            return Ok(None);
        }
        if product
            .module_interface()
            .is_none_or(|interface| interface.producer_sha256() != producer)
        {
            return Err(failure("native product has another canonical producer"));
        }
        crate::certified_products::original_execution_source_digest_with_validation(
            product, validation,
        )
        .map_err(|error| CompileError::CompilerEvidence(Box::new(error)))?;
        Ok(Some(existing))
    }

    pub(crate) fn entries(&self) -> Vec<Arc<ArtifactEntry>> {
        let state = self.lease.inventory.0.lock().expect("inventory lock");
        state.view_queries.fetch_add(1, Ordering::Relaxed);
        let entries = self
            .read_projection(&state)
            .entries
            .iter()
            .cloned()
            .collect::<Vec<_>>();
        state
            .entry_handle_copies
            .fetch_add(entries.len() as u64, Ordering::Relaxed);
        entries
    }
    /// Materialize requested selected artifacts without copying the retained
    /// inventory. The existing read projection preserves issuing overrides.
    pub(crate) fn entries_for_artifacts(
        &self,
        ids: &BTreeSet<ArtifactId>,
    ) -> Result<Vec<Arc<ArtifactEntry>>, CompileError> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let state = self.lease.inventory.0.lock().expect("inventory lock");
        state.view_queries.fetch_add(1, Ordering::Relaxed);
        let projection = self.read_projection(&state);
        let positions =
            ids.iter()
                .map(|id| {
                    // Registry bytes supply only the sorted search key. Membership
                    // and physical custody come from the selected view's entries.
                    let semantic = state.payloads.get(id).ok_or_else(|| {
                        failure("materialization artifact is outside retained view")
                    })?;
                    projection
                        .entries
                        .binary_search_by(|entry| {
                            (&entry.descriptor.owner, entry.descriptor.id)
                                .cmp(&(&semantic.descriptor.owner, *id))
                        })
                        .map_err(|_| failure("materialization artifact is outside retained view"))
                })
                .collect::<Result<Vec<_>, _>>()?;
        let entries = positions
            .into_iter()
            .map(|position| Arc::clone(&projection.entries[position]))
            .collect::<Vec<_>>();
        state
            .entry_handle_copies
            .fetch_add(entries.len() as u64, Ordering::Relaxed);
        Ok(entries)
    }
    /// Explicitly retained roots, excluding their hidden dependency closure.
    pub(crate) fn root_entries(&self) -> Vec<Arc<ArtifactEntry>> {
        let state = self.lease.inventory.0.lock().expect("inventory lock");
        state.view_queries.fetch_add(1, Ordering::Relaxed);
        let ids = self
            .roots()
            .iter()
            .copied()
            .map(InventoryNodeKey::artifact)
            .collect::<BTreeSet<_>>();
        let entries = self
            .read_projection(&state)
            .entries
            .iter()
            .filter(|entry| ids.contains(&entry.descriptor.id))
            .cloned()
            .collect::<Vec<_>>();
        state
            .entry_handle_copies
            .fetch_add(entries.len() as u64, Ordering::Relaxed);
        entries
    }
    #[cfg(test)]
    pub(crate) fn entries_for_owners(
        &self,
        owners: impl Iterator<Item = ExactModuleIdentity>,
    ) -> Result<BTreeMap<ExactModuleIdentity, Arc<ArtifactEntry>>, CompileError> {
        let state = self.lease.inventory.0.lock().expect("inventory lock");
        let projection = self.read_projection(&state);
        let owned = &projection.nodes;
        let selected_entries = projection
            .entries
            .iter()
            .map(|entry| (entry.descriptor.id, entry))
            .collect::<BTreeMap<_, _>>();
        let mut interfaces = BTreeMap::new();
        let mut native = BTreeMap::<ExactModuleIdentity, Vec<ArtifactId>>::new();
        for id in artifact_ids(owned) {
            let entry = selected_entries[&id];
            if entry.is_native() {
                native
                    .entry(entry.descriptor.owner.clone())
                    .or_default()
                    .push(id);
            } else {
                interfaces.insert(entry.descriptor.owner.clone(), id);
            }
        }
        let mut entries = BTreeMap::new();
        for owner in owners {
            let id = match native.get(&owner).map(Vec::as_slice) {
                Some([id]) => Some(*id),
                Some(_) => {
                    return Err(admission_failure(
                        ArtifactInventoryFailure::NativeOwnerAmbiguity { owner },
                    ));
                }
                None => interfaces.get(&owner).copied(),
            };
            if let Some(id) = id {
                entries.insert(owner, Arc::clone(selected_entries[&id]));
            }
        }
        state
            .entry_handle_copies
            .fetch_add(entries.len() as u64, Ordering::Relaxed);
        Ok(entries)
    }

    pub fn interface_projection(
        &self,
        owners: &[ExactModuleIdentity],
    ) -> Result<Self, CompileError> {
        let state = self.lease.inventory.0.lock().expect("inventory lock");
        let projection = self.read_projection(&state);
        let selected = projection
            .entries
            .iter()
            .filter_map(|entry| {
                (!entry.is_native())
                    .then_some((entry.descriptor.owner.clone(), entry.descriptor.id))
            })
            .collect::<BTreeMap<_, _>>();
        let roots = owners
            .iter()
            .map(|owner| {
                selected
                    .get(owner)
                    .copied()
                    .ok_or_else(|| failure("type owner outside retained view"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        drop(state);
        self.select_roots(roots)
    }

    pub(crate) fn interface_owners(&self) -> Vec<ExactInterfaceOwner> {
        let state = self.lease.inventory.0.lock().expect("inventory lock");
        state.view_queries.fetch_add(1, Ordering::Relaxed);
        self.read_projection(&state)
            .entries
            .iter()
            .filter(|entry| !entry.is_native())
            .map(|entry| ExactInterfaceOwner {
                owner: entry.descriptor.owner.clone(),
                requirements: entry.requirements.clone(),
            })
            .collect()
    }
}
impl PartialEq for ArtifactView {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.lease, &other.lease)
            || (self.entries() == other.entries()
                && self.selected_native_groups() == other.selected_native_groups())
    }
}
impl Eq for ArtifactView {}

#[cfg(test)]
mod native_history_properties;

#[cfg(test)]
mod compiler_projection_properties;

#[cfg(test)]
mod view_read_properties;

#[cfg(test)]
mod tests {
    use super::*;

    proptest::proptest! {
        #[test]
        fn complete_native_group_custody_survives_demotion_histories(
            count in 1u32..7, repeats in 1usize..12,
        ) {
            let inventory = ArtifactInventory::default();
            let original = Arc::new(issued_native_groups("CustodiedAvailable",
                (0..count).map(|ordinal| (ordinal, Vec::new())).collect(), &[], &BTreeMap::new()));
            let id = original.descriptor.id;
            let ArtifactPayload::Original(product) = &original.payload else { unreachable!() };
            let canonical = ArtifactEntry::canonical(product.module_interface().unwrap().clone()).descriptor.id;
            let roles = CompilerInputProjection::from_issued_entries(&[original.clone()]).unwrap().interface_only();
            let mut view = inventory.admit_shared(&inventory.empty_view(), vec![original]).unwrap();
            for _ in 0..repeats {
                view = inventory.empty_view().merge_compiler_inputs(
                    &CompilerInputProjection::default(), &view, &roles,
                ).unwrap().0;
                let before = ExactArtifactSelection::capture(&view);
                let proof = NativeOriginalRequirement::from_issued_group_custody(id, canonical, &view)
                    .unwrap().expect("one complete issued carrier remains available");
                proptest::prop_assert!(view.selected_native_groups().is_empty());
                proptest::prop_assert!(proof.selection.graph_selection().roots.iter()
                    .all(|root| matches!(root, ArtifactBindingNode::Artifact(_))));
                proptest::prop_assert_eq!(proof.selection.graph_selection().native_groups.iter()
                    .filter(|group| group.binding.artifact == id).count(), count as usize);
                proptest::prop_assert_eq!(ExactArtifactSelection::capture(&view), before);
            }
        }

        #[test]
        fn incomplete_native_carriers_cannot_union_their_available_census(
            count in 2u32..7, pivot in 1u32..6,
        ) {
            let pivot = 1 + (pivot % (count - 1));
            let inventory = ArtifactInventory::default();
            let original = Arc::new(issued_native_groups("SplitAvailable",
                (0..count).map(|ordinal| (ordinal, Vec::new())).collect(), &[], &BTreeMap::new()));
            let id = original.descriptor.id;
            let ArtifactPayload::Original(product) = &original.payload else { unreachable!() };
            let canonical = ArtifactEntry::canonical(product.module_interface().unwrap().clone()).descriptor.id;
            let roles = CompilerInputProjection::from_issued_entries(&[original.clone()]).unwrap().interface_only();
            let canonical_entry = Arc::new(ArtifactEntry::canonical(product.module_interface().unwrap().clone()));
            let base = inventory.admit_shared(&inventory.empty_view(), vec![canonical_entry]).unwrap();
            let canonical_binding = base.namespace()[&canonical];
            let capture = |range: std::ops::Range<u32>, name: &str| inventory.admit_recovery_selection(
                &base, vec![original.clone(), Arc::new(entry(name, &[]))],
                &range.map(|original_ordinal| NativeGroupKey { artifact: id, original_ordinal }).collect(),
            ).unwrap();
            let left = capture(0..pivot, "LeftPartition");
            let right = capture(pivot..count, "RightPartition");
            proptest::prop_assert_eq!(left.namespace()[&canonical], canonical_binding);
            proptest::prop_assert_eq!(right.namespace()[&canonical], canonical_binding);
            proptest::prop_assert_ne!(left.namespace()[&id], right.namespace()[&id]);
            let combined = left.merge_compiler_inputs(&roles, &right, &roles).unwrap().0;
            proptest::prop_assert!(combined.selected_native_groups().is_empty());
            proptest::prop_assert!(NativeOriginalRequirement::from_issued_group_custody(id, canonical, &combined)
                .unwrap().is_none());
        }
    }

    #[test]
    fn private_native_availability_resolves_exact_carrier_authority_controls() {
        let inventory = ArtifactInventory::default();
        let native = Arc::new(issued_native_groups(
            "CarrierAvailable",
            vec![(3, Vec::new()), (7, Vec::new())],
            &[],
            &BTreeMap::new(),
        ));
        let id = native.descriptor.id;
        let ArtifactPayload::Original(product) = &native.payload else {
            unreachable!()
        };
        let canonical = Arc::new(ArtifactEntry::canonical(
            product.module_interface().unwrap().clone(),
        ));
        let canonical_id = canonical.descriptor.id;
        let base = inventory
            .admit_shared(&inventory.empty_view(), vec![canonical.clone()])
            .unwrap();
        let canonical_binding = base.namespace()[&canonical_id];
        let original_roles =
            CompilerInputProjection::from_issued_entries(&[native.clone()]).unwrap();
        let interface_roles = original_roles.interface_only();
        let make = |name: &str, ordinals: &[u32]| {
            let groups = ordinals
                .iter()
                .map(|original_ordinal| NativeGroupKey {
                    artifact: id,
                    original_ordinal: *original_ordinal,
                })
                .collect();
            let view = inventory
                .admit_recovery_selection(
                    &base,
                    vec![native.clone(), Arc::new(entry(name, &[]))],
                    &groups,
                )
                .unwrap();
            assert_eq!(
                view.capture_graph_selection()
                    .native_groups
                    .iter()
                    .filter(|group| group.binding.artifact == id)
                    .map(|group| group.original_ordinal)
                    .collect::<BTreeSet<_>>(),
                ordinals.iter().copied().collect()
            );
            view
        };
        let left = make("LeftCarrier", &[3, 7]);
        let right = make("RightCarrier", &[3, 7]);
        assert_eq!(left.namespace()[&canonical_id], canonical_binding);
        assert_eq!(right.namespace()[&canonical_id], canonical_binding);
        assert_ne!(left.namespace()[&id], right.namespace()[&id]);
        let joined = left.merge(&right).unwrap();
        let ambiguous = inventory
            .empty_view()
            .merge_compiler_inputs(
                &CompilerInputProjection::default(),
                &joined,
                &interface_roles,
            )
            .unwrap()
            .0;
        assert!(
            matches!(NativeOriginalRequirement::from_issued_group_custody(id, canonical_id, &ambiguous),
            Err(CompileError::ArtifactInventory(error))
                if matches!(error.failure, ArtifactInventoryFailure::BindingAmbiguity { artifact } if artifact == id))
        );
        let chosen = left
            .merge_compiler_inputs(&original_roles, &right, &interface_roles)
            .unwrap()
            .0;
        let proof = NativeOriginalRequirement::from_issued_group_custody(id, canonical_id, &chosen)
            .unwrap()
            .unwrap();
        assert!(proof
            .selection
            .graph_selection()
            .bindings
            .contains(&left.namespace()[&id]));
        assert!(!proof
            .selection
            .graph_selection()
            .bindings
            .contains(&right.namespace()[&id]));

        let partial = make("PartialCarrier", &[3]);
        assert_eq!(partial.namespace()[&canonical_id], canonical_binding);
        let no_fallback = partial
            .merge_compiler_inputs(&original_roles, &right, &interface_roles)
            .unwrap()
            .0;
        assert_eq!(no_fallback.namespace()[&id], partial.namespace()[&id]);
        assert!(
            NativeOriginalRequirement::from_issued_group_custody(id, canonical_id, &no_fallback)
                .unwrap()
                .is_none(),
            "an explicit incomplete carrier cannot borrow another complete carrier"
        );
        let other_half = make("OtherPartialCarrier", &[7]);
        assert_eq!(other_half.namespace()[&canonical_id], canonical_binding);
        let split = partial.merge(&other_half).unwrap();
        let split = inventory
            .empty_view()
            .merge_compiler_inputs(
                &CompilerInputProjection::default(),
                &split,
                &interface_roles,
            )
            .unwrap()
            .0;
        assert!(
            NativeOriginalRequirement::from_issued_group_custody(id, canonical_id, &split)
                .unwrap()
                .is_none()
        );

        let changed = inventory
            .admit_shared(
                &inventory.empty_view(),
                vec![canonical, Arc::new(entry("AnotherCanonicalScope", &[]))],
            )
            .unwrap();
        assert_ne!(changed.namespace()[&canonical_id], canonical_binding);
        let mismatch = changed
            .merge_compiler_inputs(&interface_roles, &left, &interface_roles)
            .unwrap()
            .0;
        assert!(
            NativeOriginalRequirement::from_issued_group_custody(id, canonical_id, &mismatch)
                .unwrap()
                .is_none(),
            "equal canonical bytes cannot substitute another recorded binding"
        );
    }

    #[test]
    fn reissued_selection_observation_does_not_embed_a_large_native_graph() {
        let inventory = ArtifactInventory::default();
        let original = issued_native_groups(
            "WideObservation",
            (0..20_000).map(|ordinal| (ordinal, Vec::new())).collect(),
            &[],
            &BTreeMap::new(),
        );
        let view = inventory
            .admit(&inventory.empty_view(), vec![original])
            .unwrap();
        let selection = ExactArtifactSelection::capture(&view);
        let complete = serde_json::to_vec(&selection).unwrap();
        assert!(
            complete.len() > 16 << 20,
            "exercise the former manifest size failure"
        );
        let observation = selection.observation_digest().unwrap();
        let mut expected = Sha256::new();
        expected.update(b"Tidepool selected binding context observation v4\0");
        let graph = selection.graph_selection();
        expected.update(
            serde_json::to_vec(&(
                &graph.bindings,
                &graph.namespace,
                &graph.roots,
                &graph.native_groups,
                &graph.custody_roots,
            ))
            .unwrap(),
        );
        assert_eq!(observation.0, <[u8; 32]>::from(expected.finalize()));
        let encoded = serde_json::to_vec(&observation).unwrap();
        assert!(encoded.len() <= 129);
        assert_eq!(
            serde_json::from_slice::<ExactArtifactSelectionDigest>(&encoded).unwrap(),
            observation
        );
    }

    proptest::proptest! {
        #[test]
        fn reissued_selection_digest_covers_all_graph_observations(ordinal in 1u32..1_024) {
            let inventory = ArtifactInventory::default();
            let original = issued_native_groups("Observed", vec![(ordinal, Vec::new())], &[], &BTreeMap::new());
            let view = inventory.admit(&inventory.empty_view(), vec![original]).unwrap();
            let selection = ExactArtifactSelection::capture(&view);
            let expected = selection.observation_digest().unwrap();
            let value = serde_json::to_value(&selection).unwrap();
            let reopened: ExactArtifactSelection = serde_json::from_value(value.clone()).unwrap();
            proptest::prop_assert_eq!(reopened.observation_digest().unwrap(), expected);
            for field in ["bindings", "namespace", "roots", "native_groups", "custody_roots"] {
                let mut changed = value.clone();
                let rows = changed["graph"][field].as_array_mut().unwrap();
                if rows.is_empty() {
                    let binding = selection.graph_selection().bindings[0];
                    let added = match field {
                        "roots" | "custody_roots" =>
                            serde_json::to_value(ArtifactBindingNode::Artifact(binding)).unwrap(),
                        "native_groups" => serde_json::to_value(
                            selection.graph_selection().native_groups[0],
                        ).unwrap(),
                        "bindings" | "namespace" => serde_json::to_value(binding).unwrap(),
                        _ => unreachable!(),
                    };
                    rows.push(added);
                } else {
                    rows.pop();
                }
                let changed: ExactArtifactSelection = serde_json::from_value(changed).unwrap();
                proptest::prop_assert_ne!(changed.observation_digest().unwrap(), expected, "omitted field {}", field);
            }
            for field in ["nodes", "dependencies"] {
                let mut changed = value.clone();
                let rows = changed["graph"]["selections"][0][field].as_array_mut().unwrap();
                proptest::prop_assert!(!rows.is_empty());
                rows.pop();
                let changed: ExactArtifactSelection = serde_json::from_value(changed).unwrap();
                proptest::prop_assert_eq!(changed.observation_digest().unwrap(), expected, "proof transport is not selected semantics: {}", field);
            }
            let mut changed = value;
            changed["graph"]["selections"].as_array_mut().unwrap().clear();
            let changed: ExactArtifactSelection = serde_json::from_value(changed).unwrap();
            proptest::prop_assert_eq!(changed.observation_digest().unwrap(), expected);
        }
    }

    #[test]
    fn interrupted_materialization_waiter_exits_without_touching_producer() {
        use tidepool_extract_cmd::{
            with_compiler_transaction_cancellable, CompilerTransactionCancellation,
            CompilerTransactionClose,
        };
        let view = ArtifactInventory::default().empty_view();
        let metadata = view.metadata_snapshot();
        let held = view.lease.materialization.lock().unwrap();
        let cancellation = CompilerTransactionCancellation::new();
        let (entered, observed) = std::sync::mpsc::channel();
        let (finished, received) = std::sync::mpsc::channel();
        std::thread::scope(|scope| {
            let worker_cancellation = cancellation.clone();
            let view = &view;
            let metadata = &metadata;
            scope.spawn(move || {
                entered.send(()).unwrap();
                let result = with_compiler_transaction_cancellable(
                    worker_cancellation,
                    |_| {},
                    || {
                        view.retain_materialization(metadata, |_| {
                            panic!("cancelled waiter cannot prepare")
                        })
                    },
                );
                finished.send(result).unwrap();
            });
            observed
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
            cancellation.cancel();
            let result = received
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
            assert!(
                matches!(result.action, Err(CompileError::Io(error)) if error.kind() == std::io::ErrorKind::Interrupted)
            );
            assert_eq!(result.close, CompilerTransactionClose::NotStarted);
            assert!(held.is_empty());
            drop(held);
        });
    }

    #[test]
    fn native_requirement_index_refuses_interface_edges() {
        assert!(index_native_requirements(vec![(
            module("Support"),
            ArtifactDependency::Interface
        )])
        .is_err());
    }

    #[test]
    fn native_requirement_index_preserves_empty_sparse_and_overlapping_edges() {
        assert!(index_native_requirements(Vec::new()).unwrap().is_empty());
        let edge = (
            module("Helper"),
            ArtifactDependency::NativeGroup {
                dependent_ordinal: u32::MAX,
                required_ordinal: 7,
            },
        );
        let overlapping = (module("OtherHelper"), edge.1.clone());
        let indexed =
            index_native_requirements(vec![edge.clone(), overlapping.clone(), edge.clone()])
                .unwrap();
        assert_eq!(indexed.len(), 1);
        assert_eq!(
            indexed[&u32::MAX].iter().cloned().collect::<BTreeSet<_>>(),
            BTreeSet::from([edge, overlapping])
        );
        assert_eq!(indexed[&u32::MAX].len(), 2);
    }

    #[test]
    fn native_requirement_planning_visits_only_selected_group_edges() {
        for count in [8u32, 64, 512] {
            let inventory = ArtifactInventory::default();
            let helper =
                issued_native_groups("Helper", vec![(7, Vec::new())], &[], &BTreeMap::new());
            let consumer = issued_native_groups(
                "Consumer",
                (0..count)
                    .map(|i| (3 + 8 * i, vec![issued_source(&helper, 7)]))
                    .collect(),
                &[&helper],
                &BTreeMap::new(),
            );
            let consumer_id = consumer.descriptor.id;
            let helper_key = NativeGroupKey {
                artifact: helper.descriptor.id,
                original_ordinal: 7,
            };
            let entries = vec![Arc::new(helper), Arc::new(consumer)];
            let selected = BTreeSet::from([
                helper_key,
                NativeGroupKey {
                    artifact: consumer_id,
                    original_ordinal: 3,
                },
            ]);
            let view = inventory
                .admit_recovery_selection(&inventory.empty_view(), entries, &selected)
                .unwrap();
            assert_eq!(view.selected_native_groups(), selected);
            let examined = || {
                inventory
                    .0
                    .lock()
                    .unwrap()
                    .native_requirement_rows_examined
                    .load(Ordering::Relaxed)
            };
            assert_eq!(
                examined(),
                1,
                "unused groups must not add edge scans at count={count}"
            );
            let all = (0..count)
                .map(|i| NativeGroupKey {
                    artifact: consumer_id,
                    original_ordinal: 3 + 8 * i,
                })
                .chain([helper_key])
                .collect::<BTreeSet<_>>();
            let before = examined();
            let expanded = inventory
                .admit_recovery_selection(&view, Vec::new(), &all)
                .unwrap();
            assert_eq!(expanded.selected_native_groups(), all);
            assert_eq!(
                examined() - before,
                u64::from(count),
                "whole selection visits each edge once"
            );
            let before = examined();
            let repeated = inventory
                .admit_recovery_selection(&expanded, Vec::new(), &all)
                .unwrap();
            assert_eq!(repeated, expanded);
            assert_eq!(
                examined() - before,
                u64::from(count),
                "known nodes still validate every selected edge"
            );
        }
    }

    #[test]
    fn cold_original_admission_distinguishes_supplied_rows_from_retained_vertices() {
        let inventory = ArtifactInventory::default();
        let empty = inventory.empty_view();
        let original =
            issued_native_groups("ColdOriginal", vec![(3, Vec::new())], &[], &BTreeMap::new());
        let id = original.descriptor.id;
        let groups = original
            .native_group_ordinals
            .iter()
            .map(|ordinal| NativeGroupKey {
                artifact: id,
                original_ordinal: *ordinal,
            })
            .collect::<BTreeSet<_>>();
        assert!(!groups.is_empty());
        let planned = std::iter::once(LogicalNodeKey::Artifact(id))
            .chain(groups.iter().copied().map(LogicalNodeKey::Group))
            .map(|key| key.bind(ArtifactSelectionId([0; 32])));
        assert!(admitted_closure(&inventory.0.lock().unwrap(), planned).is_empty());
        let first = inventory.admit(&empty, vec![original.clone()]).unwrap();
        assert_eq!(first.selected_native_groups(), groups);
        assert!(first.artifact_ids().contains(&id));
        first
            .native_requirements_from_roots(&[NativeRequirementRoot::AllGroups(id)])
            .unwrap();
        let node_count = inventory.node_count();
        let reused = inventory.admit(&first, vec![original]).unwrap();
        assert_eq!(reused, first);
        assert_eq!(inventory.node_count(), node_count);
        let missing = inventory
            .admit(&reused, vec![entry("NeedsMissing", &["Missing"])])
            .unwrap_err();
        assert!(
            matches!(missing, CompileError::ArtifactInventory(ref error) if matches!(error.failure, ArtifactInventoryFailure::MissingDependency { .. }))
        );
        assert_eq!(inventory.node_count(), node_count);
        assert_eq!(first.selected_native_groups(), groups);
    }

    #[test]
    fn admitted_closure_never_adds_unknown_artifact_or_group_seeds() {
        let inventory = ArtifactInventory::default();
        let original =
            issued_native_groups("Existing", vec![(11, Vec::new())], &[], &BTreeMap::new());
        let id = original.descriptor.id;
        let retained = inventory
            .admit(&inventory.empty_view(), vec![original])
            .unwrap();
        let unknown_id = ArtifactId([0xfe; 32]);
        let unknown_group = NativeGroupKey {
            artifact: id,
            original_ordinal: u32::MAX,
        };
        let state = inventory.0.lock().unwrap();
        let known = admitted_closure(&state, retained.roots().iter().copied());
        assert!(native_groups(&known).contains(&NativeGroupKey {
            artifact: id,
            original_ordinal: 11
        }));
        let mixed = admitted_closure(
            &state,
            retained.roots().iter().copied().chain([
                LogicalNodeKey::Artifact(unknown_id).bind(ArtifactSelectionId([0; 32])),
                LogicalNodeKey::Group(unknown_group).bind(ArtifactSelectionId([0; 32])),
            ]),
        );
        assert_eq!(mixed, known);
        drop(state);
        assert!(retained.select_roots(vec![unknown_id]).is_err());
        assert!(retained
            .native_requirements_from_roots(&[NativeRequirementRoot::Group {
                artifact: id,
                original_ordinal: u32::MAX,
            }])
            .is_err());
    }

    fn bound_selection_for_entries(
        view: &ArtifactView,
        entries: &[Arc<ArtifactEntry>],
    ) -> crate::certified_products::CertifiedSourceSelection {
        let projection = CompilerInputProjection::from_issued_entries(entries).unwrap();
        crate::certified_products::CertifiedSourceSelection::from_compiler_projection(
            &projection,
            &view.metadata_snapshot(),
            &crate::recovery_artifacts::PackageInterfaceValidation::default().inventory,
        )
        .unwrap()
    }

    #[test]
    fn bound_native_demand_reuses_exact_groups_and_issues_only_new_closure() {
        for count in [2, 32, 128] {
            let inventory = ArtifactInventory::default();
            let helper = Arc::new(issued_native_groups(
                "Helper",
                vec![(7, Vec::new())],
                &[],
                &BTreeMap::new(),
            ));
            let consumer = Arc::new(issued_native_groups(
                "Consumer",
                (0..count)
                    .map(|ordinal| (ordinal, vec![issued_source(&helper, 7)]))
                    .collect(),
                &[&helper],
                &BTreeMap::new(),
            ));
            let entries = vec![helper.clone(), consumer.clone()];
            let available = inventory
                .admit_shared_with_demand(
                    &inventory.empty_view(),
                    entries.clone(),
                    NativeArtifactDemand::ScopeInterfaces,
                )
                .unwrap();
            let selected = bound_selection_for_entries(&available, &entries);
            let mut fixed = inventory
                .bind_segment(&available, entries, &selected)
                .unwrap();
            let imports = vec![issued_source(&consumer, 0)];
            let before = inventory.metrics();
            let first = inventory
                .admit_bound_target(
                    &mut fixed,
                    &inventory.empty_view(),
                    NativeArtifactDemand::CertifiedTargetImports(&imports),
                    vec![],
                    &selected,
                )
                .unwrap();
            assert_eq!(
                inventory.metrics().installed_binding_rows - before.installed_binding_rows,
                2
            );
            let groups = first.capture_graph_selection().native_groups;
            assert_eq!(groups.len(), 2);
            let consumer_group = *groups
                .iter()
                .find(|group| group.binding.artifact == consumer.descriptor.id)
                .unwrap();
            let carrier = first
                .binding_dependencies()
                .into_iter()
                .find_map(|(from, to, edge)| match (from, to, edge) {
                    (
                        ArtifactBindingNode::Group(group),
                        ArtifactBindingNode::Artifact(carrier),
                        ArtifactDependency::Interface,
                    ) if group == consumer_group => Some(carrier),
                    _ => None,
                })
                .unwrap();
            assert_ne!(carrier.selection, consumer_group.binding.selection);
            let before = inventory.metrics();
            let examined = inventory
                .0
                .lock()
                .unwrap()
                .native_requirement_rows_examined
                .load(Ordering::Relaxed);
            let repeated = inventory
                .admit_bound_target(
                    &mut fixed,
                    &first,
                    NativeArtifactDemand::CertifiedTargetImports(&imports),
                    vec![],
                    &selected,
                )
                .unwrap();
            assert_eq!(repeated.capture_graph_selection().native_groups, groups);
            assert_eq!(
                inventory.metrics().installed_binding_rows,
                before.installed_binding_rows
            );
            assert_eq!(
                inventory.metrics().canonical_comparisons,
                before.canonical_comparisons
            );
            assert_eq!(
                inventory
                    .0
                    .lock()
                    .unwrap()
                    .native_requirement_rows_examined
                    .load(Ordering::Relaxed),
                examined
            );
            let imports = vec![issued_source(&consumer, 1)];
            let next = inventory
                .admit_bound_target(
                    &mut fixed,
                    &repeated,
                    NativeArtifactDemand::CertifiedTargetImports(&imports),
                    vec![],
                    &selected,
                )
                .unwrap();
            assert_eq!(
                inventory.metrics().installed_binding_rows - before.installed_binding_rows,
                1
            );
            assert!(next
                .capture_graph_selection()
                .native_groups
                .contains(&consumer_group));
            assert_eq!(next.capture_graph_selection().native_groups.len(), 3);
        }
    }

    #[test]
    fn bound_native_demand_refuses_future_type_without_mutating_demand_or_cache() {
        use crate::certified_products::PendingImportOwner;
        let inventory = ArtifactInventory::default();
        let interface = crate::certified_products::fixture_module_interface(
            [2; 32],
            "fixture",
            "ValFuture",
            BTreeMap::new(),
        );
        let value = Arc::new(ArtifactEntry::interface(
            CertifiedJoinedInterface::from_certification(
                [2; 32],
                "fixture".into(),
                "ValFuture".into(),
                interface.interface_bytes().to_vec(),
                interface.package_imports_bytes().to_vec(),
            )
            .unwrap(),
            JoinedInterfaceRole::ValueInterface,
            vec![],
        ));
        let consumer = Arc::new(issued_native_groups(
            "Consumer",
            vec![
                (0, Vec::new()),
                (
                    1,
                    vec![PendingImportOwner::Retained {
                        identity: tidepool_repr::execution_schema::testing::identity(
                            "ValFuture",
                            "value",
                        ),
                        generation: 9,
                    }],
                ),
            ],
            &[],
            &BTreeMap::new(),
        ));
        let entries = vec![consumer.clone(), value.clone()];
        let available = inventory
            .admit_shared_with_demand(
                &inventory.empty_view(),
                entries.clone(),
                NativeArtifactDemand::ScopeInterfaces,
            )
            .unwrap();
        let all = bound_selection_for_entries(&available, &entries);
        let current = bound_selection_for_entries(&available, &[consumer.clone()]);
        let mut fixed = inventory.bind_segment(&available, entries, &all).unwrap();
        let first_imports = vec![issued_source(&consumer, 0)];
        let cursor = inventory
            .admit_bound_target(
                &mut fixed,
                &inventory.empty_view(),
                NativeArtifactDemand::CertifiedTargetImports(&first_imports),
                vec![],
                &current,
            )
            .unwrap();
        assert!(!cursor.artifact_ids().contains(&value.descriptor.id));
        let before = inventory.metrics();
        let cache_before = fixed.groups.clone();
        let future_imports = vec![issued_source(&consumer, 1)];
        assert!(inventory
            .admit_bound_target(
                &mut fixed,
                &cursor,
                NativeArtifactDemand::CertifiedTargetImports(&future_imports),
                vec![],
                &current
            )
            .is_err());
        assert_eq!(fixed.groups, cache_before);
        assert_eq!(
            inventory.metrics().installed_binding_rows,
            before.installed_binding_rows
        );
        assert_eq!(cursor.selected_native_groups().len(), 1);
        let next = inventory
            .admit_bound_target(
                &mut fixed,
                &cursor,
                NativeArtifactDemand::CertifiedTargetImports(&future_imports),
                vec![value.clone()],
                &all,
            )
            .unwrap();
        assert!(next.artifact_ids().contains(&value.descriptor.id));
        assert_eq!(next.selected_native_groups().len(), 2);
    }

    #[test]
    fn bound_monotone_group_history_only_visits_new_closure() {
        for count in [2, 32, 128] {
            let inventory = ArtifactInventory::default();
            let consumer = Arc::new(issued_native_groups(
                "Consumer",
                (0..count).map(|ordinal| (ordinal, Vec::new())).collect(),
                &[],
                &BTreeMap::new(),
            ));
            let entries = vec![consumer.clone()];
            let available = inventory
                .admit_shared_with_demand(
                    &inventory.empty_view(),
                    entries.clone(),
                    NativeArtifactDemand::ScopeInterfaces,
                )
                .unwrap();
            let selected = bound_selection_for_entries(&available, &entries);
            let mut fixed = inventory
                .bind_segment(&available, entries, &selected)
                .unwrap();
            let mut cursor = inventory.empty_view();
            for ordinal in 0..count {
                let imports = vec![issued_source(&consumer, ordinal)];
                let before = inventory.metrics();
                cursor = inventory
                    .admit_bound_target(
                        &mut fixed,
                        &cursor,
                        NativeArtifactDemand::CertifiedTargetImports(&imports),
                        vec![],
                        &selected,
                    )
                    .unwrap();
                let after = inventory.metrics();
                assert_eq!(
                    after.installed_binding_rows - before.installed_binding_rows,
                    1
                );
                if ordinal > 0 {
                    assert_eq!(
                        after.graph_visits - before.graph_visits,
                        1,
                        "history {count}, ordinal {ordinal}"
                    );
                    assert_eq!(after.entry_handle_copies - before.entry_handle_copies, 0);
                }
                let before_reads = inventory.metrics();
                // Availability and executable demand keep independent caches;
                // both read orders reuse the closure already admitted above.
                if ordinal % 2 == 0 {
                    fixed.view.selected_native_groups();
                    fixed.view.entries();
                } else {
                    fixed.view.entries();
                    fixed.view.selected_native_groups();
                }
                assert_eq!(inventory.metrics().graph_visits, before_reads.graph_visits);
                assert_eq!(
                    fixed.view.selected_native_groups().len(),
                    ordinal as usize + 1
                );
                let before = inventory.metrics();
                cursor = inventory
                    .admit_bound_target(
                        &mut fixed,
                        &cursor,
                        NativeArtifactDemand::CertifiedTargetImports(&imports),
                        vec![],
                        &selected,
                    )
                    .unwrap();
                let after = inventory.metrics();
                assert_eq!(after.graph_visits, before.graph_visits);
                assert_eq!(after.entry_handle_copies, before.entry_handle_copies);
                assert_eq!(after.canonical_comparisons, before.canonical_comparisons);
            }
            drop(fixed);
            drop(available);
            assert_eq!(cursor.selected_native_groups().len(), count as usize);
            assert_eq!(cursor.artifact_ids().len(), 2);
            drop(cursor);
            assert_eq!(inventory.node_count(), 0);
        }
    }

    #[test]
    fn original_companion_adopts_existing_canonical_owner_once() {
        let inventory = ArtifactInventory::default();
        let first = issued_native_groups("Original", vec![(0, Vec::new())], &[], &BTreeMap::new());
        let independent =
            issued_native_groups("Original", vec![(0, Vec::new())], &[], &BTreeMap::new());
        let ArtifactPayload::Original(first_product) = &first.payload else {
            unreachable!()
        };
        let canonical = Arc::new(ArtifactEntry::canonical(
            first_product.module_interface().unwrap().clone(),
        ));
        let standalone = inventory
            .admit_shared(&inventory.empty_view(), vec![canonical.clone()])
            .unwrap();
        let view = inventory.admit(&standalone, vec![independent]).unwrap();
        let owner = view
            .entries()
            .into_iter()
            .find(|entry| entry.descriptor.id == first.descriptor.id)
            .unwrap();
        let ArtifactPayload::Original(product) = &owner.payload else {
            unreachable!()
        };
        let ArtifactFactsHandle::Canonical(facts) = &canonical.facts else {
            unreachable!()
        };
        assert!(Arc::ptr_eq(
            product.module_interface().unwrap().facts(),
            facts
        ));
        assert!(product.original_native().unwrap().matches_original(product));
        let before = inventory.metrics();
        inventory.admit_shared(&view, vec![owner]).unwrap();
        let after = inventory.metrics();
        assert_eq!(after.canonical_comparisons, before.canonical_comparisons);
        assert_eq!(
            after.canonical_bytes_offered,
            before.canonical_bytes_offered
        );
    }

    #[test]
    fn bound_visibility_accepts_captured_owner_after_independent_canonicalization() {
        let inventory = ArtifactInventory::default();
        let interface = crate::certified_products::fixture_module_interface(
            [2; 32],
            "fixture",
            "Value",
            BTreeMap::new(),
        );
        let make_value = || {
            Arc::new(ArtifactEntry::interface(
                CertifiedJoinedInterface::from_certification(
                    [2; 32],
                    "fixture".into(),
                    "Value".into(),
                    interface.interface_bytes().to_vec(),
                    interface.package_imports_bytes().to_vec(),
                )
                .unwrap(),
                JoinedInterfaceRole::ValueInterface,
                vec![],
            ))
        };
        let resident = make_value();
        let captured = make_value();
        let outsider = make_value();
        let available = inventory
            .admit_shared_with_demand(
                &inventory.empty_view(),
                vec![resident.clone()],
                NativeArtifactDemand::ScopeInterfaces,
            )
            .unwrap();
        let selected = bound_selection_for_entries(&available, &[captured.clone()]);
        let mut fixed = inventory
            .bind_segment(&available, vec![captured.clone()], &selected)
            .unwrap();
        assert!(fixed.entries[&captured.descriptor.id]
            .facts
            .same_owner(&resident.facts));
        let before = inventory.metrics();
        let cursor = inventory
            .admit_bound_target(
                &mut fixed,
                &inventory.empty_view(),
                NativeArtifactDemand::ScopeInterfaces,
                vec![captured],
                &selected,
            )
            .unwrap();
        assert_eq!(
            inventory.metrics().canonical_comparisons,
            before.canonical_comparisons
        );
        assert_eq!(cursor.artifact_ids(), vec![resident.descriptor.id]);
        assert!(inventory
            .admit_bound_target(
                &mut fixed,
                &cursor,
                NativeArtifactDemand::ScopeInterfaces,
                vec![outsider],
                &selected
            )
            .is_err());
    }

    #[test]
    fn bound_projection_rebinding_preserves_issuing_none_override() {
        let cold = Arc::new(issued_native_groups(
            "Original",
            vec![(0, Vec::new())],
            &[],
            &BTreeMap::new(),
        ));
        let ArtifactPayload::Original(product) = &cold.payload else {
            unreachable!()
        };
        let mut fresh = (*cold).clone();
        fresh.payload = ArtifactPayload::Original(product.clone().with_source_sha256([1; 32]));
        let inventory = ArtifactInventory::default();
        let cursor = inventory
            .admit_shared_with_demand(
                &inventory.empty_view(),
                vec![Arc::new(fresh)],
                NativeArtifactDemand::ScopeInterfaces,
            )
            .unwrap();
        let independent = ArtifactInventory::default();
        let physical_entries = vec![cold.clone(), Arc::new(entry("Other", &[]))];
        let physical = independent
            .admit_shared_with_demand(
                &independent.empty_view(),
                physical_entries.clone(),
                NativeArtifactDemand::ScopeInterfaces,
            )
            .unwrap();
        let physical = inventory
            .recover_selection(
                &inventory.empty_view(),
                physical.entries(),
                &physical.capture_graph_selection(),
                &physical.binding_dependencies(),
            )
            .unwrap();
        assert_ne!(
            cursor.namespace()[&cold.descriptor.id],
            physical.namespace()[&cold.descriptor.id]
        );
        let selected = bound_selection_for_entries(&physical, &physical_entries);
        let mut fixed = inventory
            .bind_segment(&physical, physical_entries, &selected)
            .unwrap();
        let imports = vec![issued_source(&cold, 0)];
        let admitted = inventory
            .admit_bound_target(
                &mut fixed,
                &cursor,
                NativeArtifactDemand::CertifiedTargetImports(&imports),
                vec![],
                &selected,
            )
            .unwrap();
        let uncached = ArtifactView {
            lease: Arc::clone(&admitted.lease),
            reads: Arc::new(ViewReadCache::default()),
        };
        let cached_entries = admitted.entries();
        let raw_entries = uncached.entries();
        for (cached, raw) in cached_entries.iter().zip(&raw_entries) {
            assert!(cached.same_issuing_authority(raw));
        }
        let original = cached_entries
            .iter()
            .find(|entry| entry.descriptor.id == cold.descriptor.id)
            .unwrap();
        let ArtifactPayload::Original(product) = &original.payload else {
            unreachable!()
        };
        assert_eq!(product.source_sha256(), None);
    }

    #[test]
    fn explicit_root_promotion_matches_fresh_selection_and_recovers() {
        let inventory = ArtifactInventory::default();
        let root = entry("Root", &["Child"]);
        let child = entry("Child", &[]);
        let initial = inventory
            .admit(&inventory.empty_view(), vec![root.clone(), child.clone()])
            .unwrap();
        let projected = initial.select_roots(vec![root.descriptor.id]).unwrap();
        assert_eq!(
            projected
                .root_entries()
                .iter()
                .map(|entry| entry.descriptor.id)
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([root.descriptor.id])
        );
        let promoted = inventory.admit(&projected, vec![child.clone()]).unwrap();
        let fresh_inventory = ArtifactInventory::default();
        let fresh = fresh_inventory
            .admit(&fresh_inventory.empty_view(), vec![root, child])
            .unwrap();
        assert_eq!(
            promoted.capture_graph_selection(),
            fresh.capture_graph_selection()
        );
        assert_eq!(
            promoted
                .root_entries()
                .iter()
                .map(|entry| entry.descriptor.id)
                .collect::<BTreeSet<_>>(),
            fresh
                .root_entries()
                .iter()
                .map(|entry| entry.descriptor.id)
                .collect()
        );
        let recovered_inventory = ArtifactInventory::default();
        let recovered = recovered_inventory
            .recover_selection(
                &recovered_inventory.empty_view(),
                promoted.entries(),
                &promoted.capture_graph_selection(),
                &promoted.binding_dependencies(),
            )
            .unwrap();
        assert_eq!(
            recovered.capture_graph_selection(),
            promoted.capture_graph_selection()
        );
        drop(recovered);
        assert_eq!(recovered_inventory.node_count(), 0);
        drop(fresh);
        assert_eq!(fresh_inventory.node_count(), 0);
        drop(promoted);
        drop(projected);
        drop(initial);
        assert_eq!(inventory.node_count(), 0);
    }

    #[test]
    fn bound_resolver_refuses_ambiguous_full_native_owner_without_changing_custody() {
        let inventory = ArtifactInventory::default();
        let first = issued_native_groups("Original", vec![(0, Vec::new())], &[], &BTreeMap::new());
        let ArtifactPayload::Original(product) = &first.payload else {
            unreachable!()
        };
        let other = ArtifactEntry::original(
            [3; 32],
            crate::certified_products::fixture_finalized_product(product.clone(), [3; 32]),
        )
        .unwrap();
        let ArtifactPayload::Original(other_product) = &other.payload else {
            unreachable!()
        };
        assert_eq!(product.owner(), other_product.owner());
        assert_ne!(first.descriptor.id, other.descriptor.id);
        let left = inventory
            .admit_shared_with_demand(
                &inventory.empty_view(),
                vec![Arc::new(first)],
                NativeArtifactDemand::ScopeInterfaces,
            )
            .unwrap();
        let right = inventory
            .admit_shared_with_demand(
                &inventory.empty_view(),
                vec![Arc::new(other)],
                NativeArtifactDemand::ScopeInterfaces,
            )
            .unwrap();
        let combined = left
            .merge_selected(&right, &CompilerInputProjection::default())
            .unwrap();
        let selected = bound_selection_for_entries(&combined, &[]);
        let graph = combined.capture_graph_selection();
        let nodes = inventory.node_count();
        assert!(
            matches!(inventory.bind_segment(&combined, vec![], &selected), Err(CompileError::ArtifactInventory(error)) if matches!(error.failure, ArtifactInventoryFailure::OwnerConflict { .. }) && error.owner_conflict.is_some())
        );
        assert_eq!(combined.capture_graph_selection(), graph);
        assert_eq!(inventory.node_count(), nodes);
        let recovered_inventory = ArtifactInventory::default();
        let recovered = recovered_inventory
            .recover_selection(
                &recovered_inventory.empty_view(),
                combined.entries(),
                &graph,
                &combined.binding_dependencies(),
            )
            .unwrap();
        assert_eq!(recovered.capture_graph_selection(), graph);
        drop(recovered);
        assert_eq!(recovered_inventory.node_count(), 0);
        drop(combined);
        drop(left);
        drop(right);
        assert_eq!(inventory.node_count(), 0);
    }

    #[test]
    fn known_canonical_reuse_compares_no_payloads_and_independent_equal_ingress_adopts_owner() {
        let inventory = ArtifactInventory::default();
        let first = Arc::new(entry("Canonical", &[]));
        let view = inventory
            .admit_shared(&inventory.empty_view(), vec![first.clone()])
            .unwrap();
        let before = inventory.metrics();
        let reused = inventory.admit_shared(&view, vec![first.clone()]).unwrap();
        let after = inventory.metrics();
        assert_eq!(after.canonical_comparisons, before.canonical_comparisons);
        assert_eq!(
            after.canonical_bytes_offered,
            before.canonical_bytes_offered
        );
        assert!(after.canonical_hits > before.canonical_hits);
        let independent = Arc::new(entry("Canonical", &[]));
        assert!(!first.facts.same_owner(&independent.facts));
        let adopted = inventory.admit_shared(&reused, vec![independent]).unwrap();
        assert_eq!(
            inventory.metrics().canonical_comparisons,
            after.canonical_comparisons + 1
        );
        let owner = adopted.entries().remove(0);
        assert!(owner.facts.same_owner(&first.facts));
        let before = inventory.metrics();
        inventory.admit_shared(&adopted, vec![owner]).unwrap();
        assert_eq!(
            inventory.metrics().canonical_comparisons,
            before.canonical_comparisons
        );
        assert_eq!(
            inventory.metrics().installed_binding_rows,
            before.installed_binding_rows
        );
    }

    #[test]
    fn independent_original_ingress_adopts_witness_and_bytes_as_one_owner() {
        let inventory = ArtifactInventory::default();
        let first = issued_native_groups("Original", vec![(0, Vec::new())], &[], &BTreeMap::new());
        let independent =
            issued_native_groups("Original", vec![(0, Vec::new())], &[], &BTreeMap::new());
        assert!(!first.facts.same_owner(&independent.facts));
        let view = inventory
            .admit(&inventory.empty_view(), vec![first.clone()])
            .unwrap();
        let adopted = inventory.admit(&view, vec![independent]).unwrap();
        let owner = adopted
            .entries()
            .into_iter()
            .find(|entry| entry.descriptor.id == first.descriptor.id)
            .unwrap();
        assert!(owner.facts.same_owner(&first.facts));
        let ArtifactPayload::Original(product) = &owner.payload else {
            panic!("original owner");
        };
        let witness = product.original_native().unwrap();
        assert!(witness.matches_original(product));
        let before = inventory.metrics();
        inventory.admit_shared(&adopted, vec![owner]).unwrap();
        assert_eq!(
            inventory.metrics().canonical_comparisons,
            before.canonical_comparisons
        );
        assert_eq!(
            inventory.metrics().canonical_bytes_offered,
            before.canonical_bytes_offered
        );
    }

    #[test]
    fn canonical_producer_identity_preserves_exact_artifact_digest() {
        let raw = [3; 32];
        let identity = CanonicalProducerIdentity::from_producer_bytes(&raw);
        let expected: [u8; 32] = Sha256::digest(raw).into();
        assert_eq!(identity.sha256(), expected);
        assert_ne!(identity.sha256(), raw);
        assert_eq!(identity.hex(), format!("{:x}", Sha256::digest(raw)));
    }
    fn module(name: &str) -> ExactModuleIdentity {
        ExactModuleIdentity {
            unit: "unit".into(),
            module: name.into(),
        }
    }
    pub(super) fn entry(name: &str, requirements: &[&str]) -> ArtifactEntry {
        entry_in_unit(
            "unit",
            name,
            requirements.iter().map(|name| module(name)).collect(),
        )
    }
    fn entry_in_unit(
        unit: &str,
        name: &str,
        requirements: Vec<ExactModuleIdentity>,
    ) -> ArtifactEntry {
        let requirements = requirements
            .into_iter()
            .map(|owner| {
                (
                    (owner.unit, owner.module.clone()),
                    digest(owner.module.as_bytes()),
                )
            })
            .collect();
        ArtifactEntry::canonical(crate::certified_products::fixture_module_interface(
            [2; 32],
            unit,
            name,
            requirements,
        ))
    }

    #[test]
    fn shared_canonical_payload_preserves_each_issued_child_binding() {
        let inventory = ArtifactInventory::default();
        let empty = inventory.empty_view();
        let old_child = entry("Child", &[]);
        let ArtifactPayload::Canonical(child) = &old_child.payload else {
            unreachable!()
        };
        let new_child = ArtifactEntry::canonical(crate::certified_products::fixture_module_core(
            child,
            b"independently issued child Core".to_vec(),
        ));
        assert_eq!(
            old_child.descriptor.interface_sha256,
            new_child.descriptor.interface_sha256,
        );
        assert_ne!(old_child.descriptor.id, new_child.descriptor.id);
        let shared_root = entry("Root", &["Child"]);
        let root_id = shared_root.descriptor.id;
        let old_child_id = old_child.descriptor.id;
        let new_child_id = new_child.descriptor.id;
        let old_unread = inventory
            .admit(&empty, vec![shared_root.clone(), old_child.clone()])
            .unwrap();
        assert!(old_unread.reads.read_projection.get().is_none());
        let old = inventory
            .admit(&empty, vec![shared_root.clone(), old_child])
            .unwrap();
        let old_selected = old.select_roots(vec![root_id]).unwrap();
        assert_eq!(
            old_selected
                .artifact_ids()
                .into_iter()
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([root_id, old_child_id]),
        );
        // Both complete packets are independently valid. Their shared payload
        // cannot inherit a child binding from another issued namespace.
        let independent = ArtifactInventory::default();
        independent
            .admit(
                &independent.empty_view(),
                vec![shared_root.clone(), new_child.clone()],
            )
            .unwrap();
        let new = inventory
            .admit(&empty, vec![shared_root, new_child])
            .unwrap();
        assert_eq!(
            new.select_roots(vec![root_id])
                .unwrap()
                .artifact_ids()
                .into_iter()
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([root_id, new_child_id]),
        );
        assert!(old_unread.reads.read_projection.get().is_none());
        assert_eq!(
            old_unread
                .artifact_ids()
                .into_iter()
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([root_id, old_child_id])
        );
        drop(old_unread);
        drop(old);
        assert_eq!(
            old_selected
                .artifact_ids()
                .into_iter()
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([root_id, old_child_id]),
        );
    }

    fn native_entry(name: &str, requirements: &[&str]) -> ArtifactEntry {
        let interface = crate::certified_products::fixture_module_interface(
            [2; 32],
            "unit",
            name,
            requirements
                .iter()
                .map(|required| {
                    (
                        ("unit".into(), (*required).into()),
                        digest(required.as_bytes()),
                    )
                })
                .collect(),
        );
        let raw = crate::certified_products::tests::original_groups_fixture_in_unit_with_sites(
            "unit",
            name,
            Vec::new(),
            1,
            &BTreeMap::new(),
            interface.interface_bytes().to_vec(),
            &BTreeMap::new(),
        );
        let owner = raw.owner().clone();
        let certification = crate::certified_products::encode_home_certification_with_module(
            &owner,
            &[],
            &BTreeMap::new(),
            interface.requirements(),
            digest(interface.certificate_bytes()),
        )
        .unwrap();
        let product = CertifiedRecoveryProduct::from_certification(
            owner,
            interface.interface_bytes().to_vec(),
            raw.product_bytes().to_vec(),
            interface.package_imports_bytes().to_vec(),
            certification,
        )
        .with_module_interface(interface)
        .unwrap();
        ArtifactEntry::original([2; 32], product).unwrap()
    }

    fn issued_native_groups(
        name: &str,
        groups: Vec<(u32, Vec<crate::certified_products::PendingImportOwner>)>,
        requirements: &[&ArtifactEntry],
        packages: &BTreeMap<(String, String), crate::certified_products::PackageInterfaceWitness>,
    ) -> ArtifactEntry {
        let product =
            crate::certified_products::tests::original_groups_fixture(name, groups, 1, packages);
        let requirements = requirements
            .iter()
            .map(|entry| {
                (
                    (
                        entry.descriptor.owner.unit.clone(),
                        entry.descriptor.owner.module.clone(),
                    ),
                    entry.descriptor.interface_sha256,
                )
            })
            .collect();
        let product = crate::certified_products::fixture_finalized_product_with_requirements(
            product,
            [2; 32],
            Some(requirements),
        );
        let issued = crate::certified_products::tests::recovered_witness_fixtures(&[product])
            .remove(0)
            .product;
        ArtifactEntry::original([2; 32], issued).unwrap()
    }

    fn issued_source(
        entry: &ArtifactEntry,
        ordinal: u32,
    ) -> crate::certified_products::PendingImportOwner {
        let ArtifactPayload::Original(product) = &entry.payload else {
            unreachable!()
        };
        let occurrence = if entry.native_group_ordinals.len() == 1 && ordinal == 7 {
            "entry".into()
        } else {
            format!("entry_{ordinal}")
        };
        crate::certified_products::PendingImportOwner::Source {
            owner: product.owner().clone(),
            original_ordinal: ordinal,
            binder: tidepool_repr::execution_schema::SymbolIdentity {
                unit: product.owner().unit.clone(),
                module: product.owner().module.clone(),
                namespace: "value".into(),
                occurrence,
                record_parent: None,
            },
        }
    }

    fn native_scope_fixture() -> (ArtifactEntry, ArtifactEntry, ArtifactEntry) {
        let values = ArtifactEntry::canonical(crate::certified_products::fixture_module_interface(
            [2; 32],
            "fixture",
            "Values",
            BTreeMap::new(),
        ));
        let ArtifactPayload::Canonical(interface) = &values.payload else {
            unreachable!()
        };
        let next = ArtifactEntry::canonical(crate::certified_products::fixture_module_core(
            interface,
            b"replacement Values Core".to_vec(),
        ));
        let native = issued_native_groups(
            "Root",
            vec![(
                7,
                vec![crate::certified_products::PendingImportOwner::Retained {
                    identity: tidepool_repr::execution_schema::testing::identity("Values", "value"),
                    generation: 4,
                }],
            )],
            &[],
            &BTreeMap::new(),
        );
        (native, values, next)
    }

    fn scoped_group_child(
        view: &ArtifactView,
        group: NativeGroupBinding,
    ) -> (ArtifactBinding, ArtifactBinding) {
        let edges = view.binding_dependencies();
        let carrier = edges
            .iter()
            .find_map(|(source, target, label)| {
                (*source == ArtifactBindingNode::Group(group)
                    && *label == ArtifactDependency::Interface)
                    .then_some(*target)
            })
            .unwrap();
        let child = edges
            .iter()
            .find_map(|(source, target, label)| {
                (*source == ArtifactBindingNode::Group(group)
                    && matches!(label, ArtifactDependency::NativeBinding { .. }))
                .then_some(*target)
            })
            .unwrap();
        let (ArtifactBindingNode::Artifact(carrier), ArtifactBindingNode::Artifact(child)) =
            (carrier, child)
        else {
            panic!("fixture targets are typed artifact bindings")
        };
        (carrier, child)
    }

    #[test]
    fn repeated_admission_preserves_exact_native_scope_after_compiler_narrowing() {
        let inventory = ArtifactInventory::default();
        let (native, values, next) = native_scope_fixture();
        let carrier = inventory
            .admit_shared_with_demand(
                &inventory.empty_view(),
                vec![Arc::new(native.clone()), Arc::new(values.clone())],
                NativeArtifactDemand::ScopeInterfaces,
            )
            .unwrap();
        let old = inventory
            .admit_shared_with_demand(
                &carrier,
                vec![Arc::new(native.clone())],
                NativeArtifactDemand::CertifiedTargetImports(&[issued_source(&native, 7)]),
            )
            .unwrap();
        let old_group = old.capture_graph_selection().native_groups[0];
        let active_projection = CompilerInputProjection::from_issued_entries(&[
            Arc::new(native.clone()),
            Arc::new(next.clone()),
        ])
        .unwrap();
        let changed = inventory
            .admit_shared_with_intent(
                &old,
                vec![Arc::new(native.clone()), Arc::new(next.clone())],
                NativeArtifactDemand::AllGroups,
                ArtifactRootIntent::Projected(&active_projection, &[]),
            )
            .unwrap();
        let changed_group = changed
            .capture_graph_selection()
            .native_groups
            .into_iter()
            .find(|group| *group != old_group)
            .unwrap();
        let (old_carrier, old_child) = scoped_group_child(&changed, old_group);
        let (new_carrier, new_child) = scoped_group_child(&changed, changed_group);
        assert_eq!(old_carrier, new_carrier);
        assert_eq!(old_child.artifact, values.descriptor.id);
        assert_eq!(new_child.artifact, next.descriptor.id);
        assert_ne!(old_group.binding.selection, changed_group.binding.selection);
        assert_ne!(new_carrier.selection, changed_group.binding.selection);
        let ArtifactPayload::Original(product) = &native.payload else {
            unreachable!()
        };
        let projection = CompilerInputProjection::from_issued_entries(&[
            Arc::new(ArtifactEntry::canonical(
                product.module_interface().unwrap().clone(),
            )),
            Arc::new(next.clone()),
        ])
        .unwrap();
        let narrowed = changed.with_compiler_projection(&projection).unwrap();
        assert!(!narrowed.namespace().contains_key(&native.descriptor.id));
        let repeated = inventory
            .admit_shared_with_intent(
                &narrowed,
                vec![Arc::new(native.clone()), Arc::new(next)],
                NativeArtifactDemand::AllGroups,
                ArtifactRootIntent::Projected(&active_projection, &[]),
            )
            .unwrap();
        assert_eq!(
            repeated.capture_graph_selection().native_groups,
            changed.capture_graph_selection().native_groups,
        );
        assert_eq!(
            repeated.binding_dependencies(),
            changed.binding_dependencies()
        );
        let current = repeated
            .select_admission_roots(vec![native.descriptor.id])
            .unwrap();
        assert_eq!(
            current.capture_graph_selection().native_groups,
            vec![changed_group]
        );
        assert!(!current.artifact_ids().contains(&values.descriptor.id));
        let current_requirements = current
            .native_requirements_from_roots(&[NativeRequirementRoot::Group {
                artifact: native.descriptor.id,
                original_ordinal: 7,
            }])
            .unwrap();
        assert_eq!(
            current_requirements.bindings[0].artifact_id,
            new_child.artifact
        );
        let current_inventory = ArtifactInventory::default();
        let current_recovered = current_inventory
            .recover_selection(
                &current_inventory.empty_view(),
                current.entries(),
                &current.capture_graph_selection(),
                &current.binding_dependencies(),
            )
            .unwrap();
        assert_eq!(
            current_recovered.capture_graph_selection(),
            current.capture_graph_selection()
        );
        let raw_root = NativeRequirementRoot::Group {
            artifact: native.descriptor.id,
            original_ordinal: 7,
        };
        assert!(matches!(
            repeated.native_requirements_from_roots(&[raw_root]),
            Err(CompileError::ArtifactInventory(error))
                if matches!(error.failure, ArtifactInventoryFailure::BindingAmbiguity { .. })
        ));
        let support = repeated
            .support_without_owner(&ExactModuleIdentity {
                unit: "fixture".into(),
                module: "Generated".into(),
            })
            .unwrap();
        assert_eq!(
            support.capture_graph_selection(),
            repeated.capture_graph_selection()
        );
        let restored_inventory = ArtifactInventory::default();
        let restored = restored_inventory
            .recover_selection(
                &restored_inventory.empty_view(),
                support.entries(),
                &support.capture_graph_selection(),
                &support.binding_dependencies(),
            )
            .unwrap();
        assert_eq!(
            restored.capture_graph_selection(),
            support.capture_graph_selection()
        );
        let recovered_repeat = restored_inventory
            .admit_shared_with_intent(
                &restored,
                vec![Arc::new(native)],
                NativeArtifactDemand::AllGroups,
                ArtifactRootIntent::Projected(&active_projection, &[]),
            )
            .unwrap();
        assert_eq!(
            recovered_repeat.capture_graph_selection().native_groups,
            restored.capture_graph_selection().native_groups,
        );
    }

    fn replay_equal_payload_admission_history(choices: &[u8]) {
        let inventory = ArtifactInventory::default();
        let (_, values, next) = native_scope_fixture();
        assert_ne!(values.descriptor.id, next.descriptor.id);
        let native = issued_native_groups(
            "HistoricalAdmission",
            [3, 11, 29]
                .into_iter()
                .map(|ordinal| {
                    (
                        ordinal,
                        vec![crate::certified_products::PendingImportOwner::Retained {
                            identity: tidepool_repr::execution_schema::testing::identity(
                                "Values", "value",
                            ),
                            generation: 4,
                        }],
                    )
                })
                .collect(),
            &[],
            &BTreeMap::new(),
        );
        let available = inventory
            .admit_shared_with_demand(
                &inventory.empty_view(),
                vec![Arc::new(native.clone()), Arc::new(values.clone())],
                NativeArtifactDemand::ScopeInterfaces,
            )
            .unwrap();
        let old = inventory
            .admit_shared_with_demand(
                &available,
                vec![Arc::new(native.clone())],
                NativeArtifactDemand::CertifiedTargetImports(&[issued_source(&native, 3)]),
            )
            .unwrap();
        let old_facts = ExactArtifactSelection::capture(&old);
        let projection = CompilerInputProjection::from_issued_entries(&[
            Arc::new(native.clone()),
            Arc::new(next.clone()),
        ])
        .unwrap();
        let mut current = inventory
            .admit_shared_with_intent(
                &old,
                vec![Arc::new(native.clone()), Arc::new(next.clone())],
                NativeArtifactDemand::AllGroups,
                ArtifactRootIntent::Projected(&projection, &[]),
            )
            .unwrap();
        // Payload equality leaves historical scope and child choice independent.
        assert_eq!(
            current
                .entries()
                .iter()
                .filter(|entry| entry.descriptor.id == native.descriptor.id)
                .count(),
            1
        );
        let expected_old = BTreeSet::from([(3, values.descriptor.id)]);
        let expected_new = [3, 11, 29]
            .into_iter()
            .map(|ordinal| (ordinal, next.descriptor.id))
            .collect::<BTreeSet<_>>();
        let expected_union = expected_old
            .union(&expected_new)
            .copied()
            .collect::<BTreeSet<_>>();
        let observe = |view: &ArtifactView| {
            view.capture_graph_selection()
                .native_groups
                .into_iter()
                .filter(|group| group.binding.artifact == native.descriptor.id)
                .map(|group| {
                    (
                        group.original_ordinal,
                        scoped_group_child(view, group).1.artifact,
                    )
                })
                .collect::<BTreeSet<_>>()
        };
        let mut recovered_owners = Vec::new();
        for choice in std::iter::once(&0).chain(choices) {
            match choice % 4 {
                0 => {
                    current = current
                        .inventory()
                        .admit_shared_with_intent(
                            &current,
                            vec![Arc::new(native.clone())],
                            NativeArtifactDemand::AllGroups,
                            ArtifactRootIntent::Projected(&projection, &[]),
                        )
                        .unwrap();
                }
                1 => {
                    current = current
                        .with_compiler_projection(&projection.interface_only())
                        .unwrap();
                }
                2 => {
                    let owner = ArtifactInventory::default();
                    let restored = owner
                        .recover_selection(
                            &owner.empty_view(),
                            current.entries(),
                            &current.capture_graph_selection(),
                            &current.binding_dependencies(),
                        )
                        .unwrap();
                    current = restored;
                    recovered_owners.push(owner);
                }
                _ => {
                    current = current.merge_selected(&old, &projection).unwrap();
                }
            }
            assert_eq!(observe(&current), expected_union);
            let reopened = current
                .select_issued(vec![native.descriptor.id], &old_facts)
                .unwrap();
            assert_eq!(observe(&reopened), expected_old);
            let graph = current.capture_graph_selection();
            assert!(
                matches!(current.native_requirements_from_roots(&[NativeRequirementRoot::Group {
                artifact: native.descriptor.id, original_ordinal: 3,
            }]), Err(CompileError::ArtifactInventory(error)) if matches!(error.failure, ArtifactInventoryFailure::BindingAmbiguity { .. }))
            );
            assert!(
                matches!(current.native_requirements_from_roots(&[NativeRequirementRoot::AllGroups(ArtifactId([255; 32]))]),
                Err(CompileError::ArtifactInventory(error)) if matches!(error.failure, ArtifactInventoryFailure::NativeRootOutsideView { .. }))
            );
            let foreign = ArtifactInventory::default();
            assert!(foreign.admit(&current, vec![native.clone()]).is_err());
            assert_eq!(foreign.node_count(), 0);
            assert_eq!(current.capture_graph_selection(), graph);
            let issued = current
                .inventory()
                .admit_shared_with_intent(
                    &current,
                    vec![Arc::new(native.clone())],
                    NativeArtifactDemand::AllGroups,
                    ArtifactRootIntent::Projected(&projection, &[]),
                )
                .unwrap();
            let exact = issued
                .select_admission_roots(vec![native.descriptor.id])
                .unwrap();
            assert_eq!(observe(&exact), expected_new);
            assert_eq!(exact.capture_graph_selection().roots.len(), 4);
        }
        let missing_inventory = ArtifactInventory::default();
        let missing = missing_inventory
            .admit_shared_with_demand(
                &missing_inventory.empty_view(),
                vec![Arc::new(native.clone())],
                NativeArtifactDemand::ScopeInterfaces,
            )
            .unwrap();
        let missing_graph = missing.capture_graph_selection();
        assert!(
            matches!(missing_inventory.admit_shared_with_demand(&missing, vec![Arc::new(native)], NativeArtifactDemand::AllGroups),
            Err(CompileError::ArtifactInventory(error)) if matches!(error.failure, ArtifactInventoryFailure::MissingDependency { .. }))
        );
        assert_eq!(missing.capture_graph_selection(), missing_graph);
        drop(missing);
        assert_eq!(missing_inventory.node_count(), 0);
        drop(current);
        drop(old);
        drop(available);
        assert_eq!(inventory.node_count(), 0);
        assert!(recovered_owners.iter().all(|owner| owner.node_count() == 0));
    }

    #[test]
    fn equal_payload_admission_refusals_preserve_exact_history_and_release() {
        replay_equal_payload_admission_history(&[0, 1, 2, 3, 0, 2, 1, 0]);
    }

    #[test]
    fn equal_payload_complete_admissions_match_independent_child_history() {
        use proptest::test_runner::{
            contextualize_config, Config, FileFailurePersistence, TestRunner,
        };
        let mut config = Config::default();
        if std::env::var_os("PROPTEST_CASES").is_none() {
            config.cases = 64;
        }
        if let Some(path) = option_env!("TIDEPOOL_PROPTEST_REGRESSIONS") {
            config.failure_persistence = Some(Box::new(FileFailurePersistence::Direct(path)));
        }
        let mut config = contextualize_config(config);
        config.source_file = Some(file!());
        config.test_name = Some(concat!(
            module_path!(),
            "::equal_payload_complete_admissions_match_independent_child_history"
        ));
        let configured = config.cases;
        let callbacks = std::cell::Cell::new(0usize);
        let completed = std::cell::Cell::new(0usize);
        let operations = std::cell::Cell::new([0usize; 4]);
        let result =
            TestRunner::new(config).run(&proptest::collection::vec(0u8..4, 0..16), |choices| {
                callbacks.set(callbacks.get() + 1);
                match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    replay_equal_payload_admission_history(&choices)
                })) {
                    Ok(()) => {
                        completed.set(completed.get() + 1);
                        let mut counts = operations.get();
                        for choice in choices {
                            counts[usize::from(choice)] += 1;
                        }
                        operations.set(counts);
                        Ok(())
                    }
                    Err(_) => Err(proptest::test_runner::TestCaseError::fail(format!(
                        "historical choices={choices:?}"
                    ))),
                }
            });
        eprintln!(
            "equal payload admission: configured={configured}, callbacks={}, completed={}, operation_counts={:?}; each case covers ambiguity, foreign inventory, missing dependency and release",
            callbacks.get(),
            completed.get(),
            operations.get()
        );
        result.unwrap();
    }

    #[test]
    fn equal_native_rows_in_distinct_scopes_do_not_choose_an_arbitrary_handle() {
        let inventory = ArtifactInventory::default();
        let (native, values, _) = native_scope_fixture();
        let carrier = inventory
            .admit_shared_with_demand(
                &inventory.empty_view(),
                vec![Arc::new(native.clone()), Arc::new(values)],
                NativeArtifactDemand::ScopeInterfaces,
            )
            .unwrap();
        let demand = [issued_source(&native, 7)];
        let first = inventory
            .admit_shared_with_demand(
                &carrier,
                vec![Arc::new(native.clone())],
                NativeArtifactDemand::CertifiedTargetImports(&demand),
            )
            .unwrap();
        let second = inventory
            .admit_shared_with_demand(
                &carrier,
                vec![Arc::new(native.clone()), Arc::new(entry("Unrelated", &[]))],
                NativeArtifactDemand::CertifiedTargetImports(&demand),
            )
            .unwrap();
        let first_group = first.capture_graph_selection().native_groups[0];
        let second_group = second.capture_graph_selection().native_groups[0];
        assert_ne!(first_group, second_group);
        assert_eq!(
            scoped_group_child(&first, first_group),
            scoped_group_child(&second, second_group),
        );
        for joined in [first.merge(&second).unwrap(), second.merge(&first).unwrap()] {
            assert!(matches!(
                joined.select_roots(vec![native.descriptor.id]),
                Err(CompileError::ArtifactInventory(error))
                    if matches!(error.failure, ArtifactInventoryFailure::BindingAmbiguity { .. })
            ));
            let issued = inventory.admit(&joined, vec![native.clone()]).unwrap();
            assert_eq!(
                issued.capture_graph_selection().native_groups,
                joined.capture_graph_selection().native_groups,
                "new scope identity is issued from rows, never join iteration order",
            );
            let current = issued
                .select_admission_roots(vec![native.descriptor.id])
                .unwrap();
            assert_eq!(
                current.capture_graph_selection().native_groups,
                vec![first_group]
            );
            let old = joined
                .select_issued(
                    vec![native.descriptor.id],
                    &ExactArtifactSelection::capture(&first),
                )
                .unwrap();
            assert_eq!(
                old.capture_graph_selection(),
                first.capture_graph_selection()
            );
        }
    }

    #[test]
    fn native_group_reuse_compares_equal_payload_children_by_exact_binding() {
        let inventory = ArtifactInventory::default();
        let (native, _, _) = native_scope_fixture();
        let child = entry_in_unit("fixture", "Child", vec![]);
        let values = entry_in_unit(
            "fixture",
            "Values",
            vec![ExactModuleIdentity {
                unit: "fixture".into(),
                module: "Child".into(),
            }],
        );
        let ArtifactPayload::Canonical(interface) = &child.payload else {
            unreachable!()
        };
        let next_child = ArtifactEntry::canonical(crate::certified_products::fixture_module_core(
            interface,
            b"next exact Child Core".to_vec(),
        ));
        let old = inventory
            .admit(
                &inventory.empty_view(),
                vec![native.clone(), values.clone(), child],
            )
            .unwrap();
        let old_group = old.capture_graph_selection().native_groups[0];
        let projection = CompilerInputProjection::from_issued_entries(&[
            Arc::new(native.clone()),
            Arc::new(values),
            Arc::new(next_child.clone()),
        ])
        .unwrap();
        let new = inventory
            .admit_projected_with_native_requirements(
                &old,
                vec![Arc::new(next_child)],
                &projection,
                &[],
            )
            .unwrap();
        let new_group = new
            .capture_graph_selection()
            .native_groups
            .into_iter()
            .find(|group| *group != old_group)
            .unwrap();
        let (old_carrier, old_values) = scoped_group_child(&new, old_group);
        let (new_carrier, new_values) = scoped_group_child(&new, new_group);
        assert_eq!(old_carrier, new_carrier);
        assert_eq!(old_values.artifact, new_values.artifact);
        assert_ne!(old_values.selection, new_values.selection);
        let repeated = inventory
            .admit_shared_with_intent(
                &new,
                vec![Arc::new(native.clone())],
                NativeArtifactDemand::AllGroups,
                ArtifactRootIntent::Projected(&projection, &[]),
            )
            .unwrap();
        assert_eq!(
            repeated.capture_graph_selection().native_groups,
            new.capture_graph_selection().native_groups
        );
        let selected = repeated
            .select_admission_roots(vec![native.descriptor.id])
            .unwrap();
        assert_eq!(
            selected.capture_graph_selection().native_groups,
            vec![new_group]
        );
        assert_eq!(scoped_group_child(&selected, new_group).1, new_values);
        let recovered_inventory = ArtifactInventory::default();
        let recovered = recovered_inventory
            .recover_selection(
                &recovered_inventory.empty_view(),
                selected.entries(),
                &selected.capture_graph_selection(),
                &selected.binding_dependencies(),
            )
            .unwrap();
        assert_eq!(scoped_group_child(&recovered, new_group).1, new_values);
    }

    #[test]
    fn published_original_admission_preserves_issuer_choice_over_equivalent_custody() {
        use crate::declaration_context::ExactDeclarationContext;
        use crate::declaration_join::ExactLexicalNode;

        let inventory = ArtifactInventory::default();
        let (native, values, next_values) = native_scope_fixture();
        let target = entry_in_unit("fixture", "Target", vec![]);
        let carriers = inventory
            .admit_shared_with_demand(
                &inventory.empty_view(),
                vec![
                    Arc::new(native.clone()),
                    Arc::new(values.clone()),
                    Arc::new(target.clone()),
                ],
                NativeArtifactDemand::ScopeInterfaces,
            )
            .unwrap();
        let first = inventory
            .admit_shared_with_demand(
                &carriers,
                vec![Arc::new(native.clone())],
                NativeArtifactDemand::AllGroups,
            )
            .unwrap();
        let second = inventory
            .admit_shared_with_demand(
                &carriers,
                vec![
                    Arc::new(native.clone()),
                    Arc::new(entry_in_unit("fixture", "Unrelated", vec![])),
                ],
                NativeArtifactDemand::AllGroups,
            )
            .unwrap();
        let groups = [
            first.capture_graph_selection().native_groups[0],
            second.capture_graph_selection().native_groups[0],
        ];
        assert_ne!(groups[0], groups[1]);
        assert_eq!(
            scoped_group_child(&first, groups[0]),
            scoped_group_child(&second, groups[1])
        );
        let projection = CompilerInputProjection::from_issued_entries(&[
            Arc::new(native.clone()),
            Arc::new(values),
            Arc::new(target.clone()),
        ])
        .unwrap();
        for (issuer, group) in [(&first, groups[0]), (&second, groups[1])] {
            let lexical = vec![
                ExactLexicalNode {
                    owner: target.descriptor.owner.clone(),
                    imports: vec![native.descriptor.owner.clone()],
                },
                ExactLexicalNode {
                    owner: native.descriptor.owner.clone(),
                    imports: vec![next_values.descriptor.owner.clone()],
                },
                ExactLexicalNode {
                    owner: next_values.descriptor.owner.clone(),
                    imports: vec![],
                },
            ];
            let issued = ExactDeclarationContext::from_authenticated_execution(
                [2; 32],
                issuer,
                projection.clone(),
                lexical.clone(),
                target.descriptor.owner.clone(),
                &lexical
                    .iter()
                    .map(|node| node.owner.clone())
                    .collect::<Vec<_>>(),
            )
            .unwrap();
            let publication = issued
                .issue_published_source_original("revision", "input", &native.descriptor.owner)
                .unwrap();
            let published = publication.context();
            assert_eq!(
                published
                    .artifact_view()
                    .capture_graph_selection()
                    .native_groups,
                vec![group]
            );
            let published_projection = published.compiler_input_projection();
            for joined in [first.merge(&second).unwrap(), second.merge(&first).unwrap()] {
                let scopes_before = inventory.0.lock().unwrap().selection_plans.len();
                let admitted = inventory
                    .admit_projected_with_native_requirements(
                        &joined,
                        vec![Arc::new(native.clone())],
                        published_projection,
                        &[],
                    )
                    .unwrap();
                assert_eq!(
                    inventory.0.lock().unwrap().selection_plans.len(),
                    scopes_before
                );
                let closed = admitted
                    .select_admission_roots(vec![native.descriptor.id])
                    .unwrap();
                assert_eq!(closed.capture_graph_selection().native_groups, vec![group]);
                assert_eq!(
                    scoped_group_child(&closed, group),
                    scoped_group_child(issuer, group)
                );
                let mut conflicting = CompilerInputProjection::from_issued_entries(&[
                    Arc::new(native.clone()),
                    Arc::new(next_values.clone()),
                ])
                .unwrap();
                let published_role = published_projection
                    .roles()
                    .into_iter()
                    .find(CompilerInputRole::is_published_source_original)
                    .unwrap();
                conflicting
                    .admit_role(native.descriptor.owner.clone(), published_role)
                    .unwrap();
                let unchanged = admitted.capture_graph_selection();
                let nodes_before = inventory.node_count();
                assert!(matches!(
                    inventory.admit_projected_with_native_requirements(&admitted, vec![Arc::new(next_values.clone())], &conflicting, &[]),
                    Err(CompileError::ArtifactInventory(error))
                        if error.failure == ArtifactInventoryFailure::NativeGroupSelectionConflict {
                            artifact: native.descriptor.id,
                            original_ordinal: group.original_ordinal,
                        }
                ));
                assert_eq!(inventory.node_count(), nodes_before);
                assert_eq!(
                    inventory.0.lock().unwrap().selection_plans.len(),
                    scopes_before
                );
                assert_eq!(admitted.capture_graph_selection(), unchanged);
            }
        }
    }

    #[test]
    fn published_original_admission_keeps_transitive_exact_group_choices() {
        use crate::declaration_context::ExactDeclarationContext;
        use crate::declaration_join::ExactLexicalNode;

        let inventory = ArtifactInventory::default();
        let child = issued_native_groups("Child", vec![(7, vec![])], &[], &BTreeMap::new());
        let root = issued_native_groups(
            "Root",
            vec![(7, vec![issued_source(&child, 7)])],
            &[&child],
            &BTreeMap::new(),
        );
        let (_, unrelated_values, _) = native_scope_fixture();
        let unrelated = issued_native_groups(
            "UnrelatedNative",
            vec![(
                19,
                vec![crate::certified_products::PendingImportOwner::Retained {
                    identity: tidepool_repr::execution_schema::testing::identity("Values", "value"),
                    generation: 4,
                }],
            )],
            &[],
            &BTreeMap::new(),
        );
        let target = entry_in_unit("fixture", "Target", vec![]);
        let carriers = inventory
            .admit_shared_with_demand(
                &inventory.empty_view(),
                vec![
                    Arc::new(root.clone()),
                    Arc::new(child.clone()),
                    Arc::new(target.clone()),
                ],
                NativeArtifactDemand::ScopeInterfaces,
            )
            .unwrap();
        let original = inventory
            .admit_shared_with_demand(
                &carriers,
                vec![Arc::new(root.clone())],
                NativeArtifactDemand::AllGroups,
            )
            .unwrap();
        let sibling = inventory
            .admit_shared_with_demand(
                &carriers,
                vec![
                    Arc::new(child.clone()),
                    Arc::new(entry_in_unit("fixture", "Unrelated", vec![])),
                    Arc::new(unrelated.clone()),
                    Arc::new(unrelated_values.clone()),
                ],
                NativeArtifactDemand::AllGroups,
            )
            .unwrap();
        let original_child = original
            .capture_graph_selection()
            .native_groups
            .into_iter()
            .find(|group| group.binding.artifact == child.descriptor.id)
            .unwrap();
        let sibling_child = sibling
            .capture_graph_selection()
            .native_groups
            .into_iter()
            .find(|group| group.binding.artifact == child.descriptor.id)
            .unwrap();
        assert_ne!(original_child, sibling_child);
        let projection = CompilerInputProjection::from_issued_entries(&[
            Arc::new(root.clone()),
            Arc::new(child.clone()),
            Arc::new(target.clone()),
        ])
        .unwrap();
        let lexical = vec![
            ExactLexicalNode {
                owner: target.descriptor.owner.clone(),
                imports: vec![root.descriptor.owner.clone()],
            },
            ExactLexicalNode {
                owner: root.descriptor.owner.clone(),
                imports: vec![child.descriptor.owner.clone()],
            },
            ExactLexicalNode {
                owner: child.descriptor.owner.clone(),
                imports: vec![],
            },
        ];
        for projection in [
            projection.clone(),
            projection.for_source_owners(&BTreeSet::from([root.descriptor.owner.clone()])),
        ] {
            let child_reusable = projection
                .roles()
                .iter()
                .any(|role| role.original() == Some(child.descriptor.id));
            let issued = ExactDeclarationContext::from_authenticated_execution(
                [2; 32],
                &original,
                projection,
                lexical.clone(),
                target.descriptor.owner.clone(),
                &lexical
                    .iter()
                    .map(|node| node.owner.clone())
                    .collect::<Vec<_>>(),
            )
            .unwrap();
            let publication = issued
                .issue_published_source_original("revision", "input", &root.descriptor.owner)
                .unwrap();
            let published = publication.context();
            let validation = crate::recovery_artifacts::PackageInterfaceValidation::default();
            let certified =
                crate::certified_products::CertifiedSourceSelection::from_compiler_projection(
                    published.compiler_input_projection(),
                    &published.artifact_view().metadata_snapshot(),
                    &validation.inventory,
                )
                .unwrap();
            let expected = published
                .artifact_view()
                .capture_graph_selection()
                .native_groups;
            assert!(expected.contains(&original_child));
            assert!(!expected.contains(&sibling_child));
            assert!(!expected
                .iter()
                .any(|group| group.binding.artifact == unrelated.descriptor.id));
            for (joined, use_certified) in [
                original.merge(&sibling).unwrap(),
                sibling.merge(&original).unwrap(),
            ]
            .into_iter()
            .flat_map(|joined| [(joined.clone(), false), (joined, true)])
            {
                let groups = joined.capture_graph_selection().native_groups;
                assert!(groups.contains(&original_child));
                assert!(groups.contains(&sibling_child));
                let scopes_before = inventory.0.lock().unwrap().selection_plans.len();
                let admitted = if use_certified {
                    inventory.admit_certified_with_demand(
                        &joined,
                        vec![Arc::new(root.clone())],
                        NativeArtifactDemand::ScopeInterfaces,
                        &certified,
                    )
                } else {
                    inventory.admit_projected_with_native_requirements(
                        &joined,
                        vec![Arc::new(root.clone())],
                        published.compiler_input_projection(),
                        &[],
                    )
                }
                .unwrap();
                assert_eq!(
                    admitted.namespace().contains_key(&child.descriptor.id),
                    child_reusable
                );
                assert!(!admitted.namespace().contains_key(&unrelated.descriptor.id));
                assert!(!admitted
                    .namespace()
                    .contains_key(&unrelated_values.descriptor.id));
                assert!(!admitted
                    .lease
                    .roots
                    .iter()
                    .any(|node| node.artifact() == unrelated.descriptor.id));
                assert!(admitted.artifact_ids().contains(&unrelated.descriptor.id));
                let selected = admitted
                    .select_admission_roots(vec![root.descriptor.id])
                    .unwrap();
                assert_eq!(selected.capture_graph_selection().native_groups, expected);
                assert_eq!(
                    inventory.0.lock().unwrap().selection_plans.len(),
                    scopes_before
                );
                let recovered_inventory = ArtifactInventory::default();
                let recovered = recovered_inventory
                    .recover_selection(
                        &recovered_inventory.empty_view(),
                        joined.entries(),
                        &joined.capture_graph_selection(),
                        &joined.binding_dependencies(),
                    )
                    .unwrap();
                let repeated = if use_certified {
                    recovered_inventory.admit_certified_with_demand(
                        &recovered,
                        vec![Arc::new(root.clone())],
                        NativeArtifactDemand::ScopeInterfaces,
                        &certified,
                    )
                } else {
                    recovered_inventory.admit_projected_with_native_requirements(
                        &recovered,
                        vec![Arc::new(root.clone())],
                        published.compiler_input_projection(),
                        &[],
                    )
                }
                .unwrap();
                assert_eq!(
                    repeated
                        .select_admission_roots(vec![root.descriptor.id])
                        .unwrap()
                        .capture_graph_selection()
                        .native_groups,
                    expected
                );
            }
        }
    }

    #[test]
    fn private_original_requirement_survives_receipt_with_interface_only_children() {
        let inventory = ArtifactInventory::default();
        let child = issued_native_groups("PrivateChild", vec![(7, vec![])], &[], &BTreeMap::new());
        let root = issued_native_groups(
            "PrivateRoot",
            vec![(12, vec![issued_source(&child, 7)])],
            &[&child],
            &BTreeMap::new(),
        );
        let unrelated = issued_native_groups(
            "PrivateUnrelated",
            vec![(19, vec![])],
            &[],
            &BTreeMap::new(),
        );
        let carriers = inventory
            .admit_shared_with_demand(
                &inventory.empty_view(),
                vec![Arc::new(root.clone()), Arc::new(child.clone())],
                NativeArtifactDemand::ScopeInterfaces,
            )
            .unwrap();
        let issuer = inventory
            .admit_shared_with_demand(
                &carriers,
                vec![Arc::new(root.clone())],
                NativeArtifactDemand::AllGroups,
            )
            .unwrap();
        let sibling = inventory
            .admit_shared_with_demand(
                &carriers,
                vec![Arc::new(child.clone()), Arc::new(unrelated.clone())],
                NativeArtifactDemand::AllGroups,
            )
            .unwrap();
        let expected = issuer.capture_graph_selection().native_groups;
        let ordinary = CompilerInputProjection::from_issued_entries(&[
            Arc::new(root.clone()),
            Arc::new(child.clone()),
        ])
        .unwrap();
        let validation = crate::recovery_artifacts::PackageInterfaceValidation::default();
        for projection in [
            ordinary.clone(),
            ordinary.for_source_owners(&BTreeSet::from([root.descriptor.owner.clone()])),
        ] {
            assert!(!projection
                .roles()
                .iter()
                .any(CompilerInputRole::is_published_source_original));
            let child_reusable = projection
                .roles()
                .iter()
                .any(|role| role.original() == Some(child.descriptor.id));
            let facts =
                NativeOriginalRequirement::for_projection(&projection, &issuer, &[]).unwrap();
            let mut receipt =
                crate::certified_products::CertifiedSourceSelection::from_compiler_projection(
                    &projection,
                    &issuer.metadata_snapshot(),
                    &validation.inventory,
                )
                .unwrap();
            if !child_reusable {
                let broader = issuer.merge(&sibling).unwrap();
                assert!(matches!(inventory.admit_certified_with_demand(
                    &broader, vec![Arc::new(root.clone())], NativeArtifactDemand::ScopeInterfaces, &receipt,
                ), Err(CompileError::ArtifactInventory(error)) if matches!(error.failure,
                    ArtifactInventoryFailure::MissingDependency { ref required, dependency: ArtifactDependency::NativeGroup { dependent_ordinal: 12, required_ordinal: 7 }, .. }
                    if required == &child.descriptor.owner)));
            }
            receipt
                .with_native_original_requirements(&issuer.metadata_snapshot(), &facts)
                .unwrap();
            for (joined, projected) in [
                issuer.merge(&sibling).unwrap(),
                sibling.merge(&issuer).unwrap(),
            ]
            .into_iter()
            .flat_map(|joined| [(joined.clone(), false), (joined, true)])
            {
                let scopes_before = inventory.0.lock().unwrap().selection_plans.len();
                let admitted = if projected {
                    inventory.admit_projected_with_native_requirements(
                        &joined,
                        vec![Arc::new(root.clone())],
                        &projection,
                        &facts,
                    )
                } else {
                    inventory.admit_certified_with_demand(
                        &joined,
                        vec![Arc::new(root.clone())],
                        NativeArtifactDemand::ScopeInterfaces,
                        &receipt,
                    )
                }
                .unwrap();
                let closed = admitted
                    .select_admission_roots(vec![root.descriptor.id])
                    .unwrap();
                assert_eq!(closed.capture_graph_selection().native_groups, expected);
                assert_eq!(
                    closed.namespace().contains_key(&child.descriptor.id),
                    child_reusable
                );
                assert!(!closed.artifact_ids().contains(&unrelated.descriptor.id));
                assert_eq!(
                    inventory.0.lock().unwrap().selection_plans.len(),
                    scopes_before
                );
                let cold = ArtifactInventory::default();
                let reopened = cold
                    .recover_selection(
                        &cold.empty_view(),
                        joined.entries(),
                        &joined.capture_graph_selection(),
                        &joined.binding_dependencies(),
                    )
                    .unwrap();
                let readmitted = if projected {
                    cold.admit_projected_with_native_requirements(
                        &reopened,
                        vec![Arc::new(root.clone())],
                        &projection,
                        &facts,
                    )
                } else {
                    cold.admit_certified_with_demand(
                        &reopened,
                        vec![Arc::new(root.clone())],
                        NativeArtifactDemand::ScopeInterfaces,
                        &receipt,
                    )
                }
                .unwrap();
                let repeated = readmitted
                    .select_admission_roots(vec![root.descriptor.id])
                    .unwrap();
                assert_eq!(repeated.capture_graph_selection().native_groups, expected);
            }
        }
        assert!(
            NativeOriginalRequirement::from_selected_view(root.descriptor.id, &carriers)
                .unwrap()
                .is_none()
        );
        let projection =
            ordinary.for_source_owners(&BTreeSet::from([root.descriptor.owner.clone()]));
        let mut facts =
            NativeOriginalRequirement::for_projection(&projection, &issuer, &[]).unwrap();
        let alternate = inventory
            .admit_shared_with_demand(
                &carriers,
                vec![Arc::new(root.clone()), Arc::new(unrelated.clone())],
                NativeArtifactDemand::AllGroups,
            )
            .unwrap();
        facts.extend(
            NativeOriginalRequirement::for_projection(&projection, &alternate, &[]).unwrap(),
        );
        let joined = issuer.merge(&alternate).unwrap();
        let preserved =
            NativeOriginalRequirement::for_projection(&projection, &joined, &facts).unwrap();
        assert_eq!(preserved.len(), 2);
        let mut ambiguous =
            crate::certified_products::CertifiedSourceSelection::from_compiler_projection(
                &projection,
                &joined.metadata_snapshot(),
                &validation.inventory,
            )
            .unwrap();
        ambiguous
            .with_native_original_requirements(&joined.metadata_snapshot(), &preserved)
            .unwrap();
        assert!(matches!(inventory.admit_certified_with_demand(
            &joined, vec![Arc::new(root.clone())], NativeArtifactDemand::ScopeInterfaces, &ambiguous,
        ), Err(CompileError::ArtifactInventory(error)) if matches!(error.failure, ArtifactInventoryFailure::BindingAmbiguity { .. })));
    }

    #[test]
    fn value_interface_successors_keep_private_exact_native_requirements() {
        use crate::declaration_context::ExactDeclarationContext;
        use crate::declaration_join::ExactLexicalNode;
        use crate::recovery_artifacts::CertifiedValueInterface;

        let inventory = ArtifactInventory::default();
        let child = issued_native_groups("ValueChild", vec![(7, vec![])], &[], &BTreeMap::new());
        let root = issued_native_groups(
            "ValueRoot",
            vec![(12, vec![issued_source(&child, 7)])],
            &[&child],
            &BTreeMap::new(),
        );
        let issuer = inventory
            .admit(&inventory.empty_view(), vec![root.clone(), child.clone()])
            .unwrap();
        let projection = CompilerInputProjection::from_issued_entries(&[
            Arc::new(root.clone()),
            Arc::new(child.clone()),
        ])
        .unwrap()
        .for_source_owners(&BTreeSet::from([root.descriptor.owner.clone()]));
        assert!(!projection.roles().iter().any(|role| {
            role.is_published_source_original() || role.original() == Some(child.descriptor.id)
        }));
        let make_context = |view: &ArtifactView| {
            ExactDeclarationContext::from_authenticated_execution(
                [2; 32],
                view,
                projection.clone(),
                vec![ExactLexicalNode {
                    owner: root.descriptor.owner.clone(),
                    imports: Vec::new(),
                }],
                root.descriptor.owner.clone(),
                &[],
            )
            .unwrap()
        };
        let make_value = |generation, requirements| {
            let canonical = crate::certified_products::fixture_module_interface(
                [2; 32],
                &root.descriptor.owner.unit,
                &format!("Tidepool.Session.Val.G{generation}"),
                BTreeMap::new(),
            );
            Arc::new(
                CertifiedValueInterface::from_checked_compilation(
                    [2; 32],
                    ExactModuleIdentity {
                        unit: canonical.unit().into(),
                        module: canonical.module().into(),
                    },
                    canonical.interface_bytes().to_vec(),
                    canonical.package_imports_bytes().to_vec(),
                    requirements,
                )
                .unwrap(),
            )
        };
        let extend = |context: ExactDeclarationContext,
                      program: bool,
                      value: Arc<CertifiedValueInterface>| {
            if program {
                context.extend_program_value_interface(value)
            } else {
                let lexical = context.lexical_graph().to_vec();
                context.extend_with_value_interfaces(&[value], lexical)
            }
        };
        let expected = issuer.capture_graph_selection().native_groups;
        let expected_edges = issuer.binding_dependencies();
        let mut observations = Vec::new();
        for cold_start in [false, true] {
            let cold = ArtifactInventory::default();
            let view = if cold_start {
                cold.recover_selection(
                    &cold.empty_view(),
                    issuer.entries(),
                    &issuer.capture_graph_selection(),
                    &issuer.binding_dependencies(),
                )
                .unwrap()
            } else {
                issuer.clone()
            };
            for program in [false, true] {
                let first = make_value(77, vec![root.descriptor.owner.clone()]);
                let following = match extend(make_context(&view), program, first.clone()) {
                    Ok(context) => context,
                    Err(error) => {
                        eprintln!(
                            "value_extension cold_start={cold_start} program={program}: {error:?}"
                        );
                        observations.push(false);
                        continue;
                    }
                };
                let second = make_value(
                    78,
                    vec![
                        root.descriptor.owner.clone(),
                        ExactModuleIdentity {
                            unit: first.interface().unit().into(),
                            module: first.interface().module().into(),
                        },
                    ],
                );
                let following = extend(following, program, second).unwrap();
                let graph = following.artifact_view().capture_graph_selection();
                let repeated = following
                    .clone()
                    .extend_with_value_interfaces(&[first], following.lexical_graph().to_vec())
                    .unwrap();
                assert_eq!(repeated.artifact_view().capture_graph_selection(), graph);
                assert_eq!(graph.native_groups, expected);
                assert!(expected_edges.iter().all(|edge| {
                    following
                        .artifact_view()
                        .binding_dependencies()
                        .contains(edge)
                }));
                assert!(!following
                    .compiler_input_roles()
                    .iter()
                    .any(|role| { role.original() == Some(child.descriptor.id) }));
                assert!(!following
                    .lexical_graph()
                    .iter()
                    .any(|node| node.owner == child.descriptor.owner));
                assert_eq!(
                    following
                        .artifact_view()
                        .namespace()
                        .get(&root.descriptor.id),
                    issuer.namespace().get(&root.descriptor.id),
                );
                let reopened_inventory = ArtifactInventory::default();
                let reopened = reopened_inventory
                    .recover_selection(
                        &reopened_inventory.empty_view(),
                        following.artifact_view().entries(),
                        &graph,
                        &following.artifact_view().binding_dependencies(),
                    )
                    .unwrap();
                assert_eq!(reopened.capture_graph_selection(), graph);
                observations.push(true);
            }
        }
        assert_eq!(observations, vec![true; 4]);
    }

    #[test]
    fn private_native_closure_keeps_hidden_interfaces_without_sibling_group_demand() {
        let mut observations = Vec::new();
        for hidden_interfaces in [true, false] {
            let inventory = ArtifactInventory::default();
            let extra =
                issued_native_groups("NativeExtra", vec![(7, vec![])], &[], &BTreeMap::new());
            let child = issued_native_groups(
                "NativeChild",
                vec![(7, vec![]), (9, vec![issued_source(&extra, 7)])],
                &[&extra],
                &BTreeMap::new(),
            );
            let root = issued_native_groups(
                "NativeRoot",
                vec![(12, vec![issued_source(&child, 7)])],
                &[],
                &BTreeMap::new(),
            );
            let carriers = inventory
                .admit_shared_with_demand(
                    &inventory.empty_view(),
                    vec![
                        Arc::new(root.clone()),
                        Arc::new(child.clone()),
                        Arc::new(extra.clone()),
                    ],
                    NativeArtifactDemand::ScopeInterfaces,
                )
                .unwrap();
            let issuer = inventory
                .admit_shared_with_demand(
                    &carriers,
                    vec![Arc::new(root.clone())],
                    NativeArtifactDemand::AllGroups,
                )
                .unwrap();
            let sibling = inventory
                .admit_shared_with_demand(
                    &carriers,
                    vec![Arc::new(child.clone())],
                    NativeArtifactDemand::AllGroups,
                )
                .unwrap();
            let expected = issuer.capture_graph_selection().native_groups;
            assert_eq!(
                expected
                    .iter()
                    .map(|group| group.original_ordinal)
                    .collect::<BTreeSet<_>>(),
                BTreeSet::from([7, 12])
            );
            let projection = if hidden_interfaces {
                CompilerInputProjection::from_issued_entries(&[Arc::new(root.clone())]).unwrap()
            } else {
                CompilerInputProjection::from_issued_entries(&[
                    Arc::new(root.clone()),
                    Arc::new(child.clone()),
                    Arc::new(extra.clone()),
                ])
                .unwrap()
                .for_source_owners(&BTreeSet::from([root.descriptor.owner.clone()]))
            };
            let facts =
                NativeOriginalRequirement::for_projection(&projection, &issuer, &[]).unwrap();
            let validation = crate::recovery_artifacts::PackageInterfaceValidation::default();
            let mut receipt =
                crate::certified_products::CertifiedSourceSelection::from_compiler_projection(
                    &projection,
                    &issuer.metadata_snapshot(),
                    &validation.inventory,
                )
                .unwrap();
            receipt
                .with_native_original_requirements(&issuer.metadata_snapshot(), &facts)
                .unwrap();
            for (joined, projected) in [
                issuer.merge(&sibling).unwrap(),
                sibling.merge(&issuer).unwrap(),
            ]
            .into_iter()
            .flat_map(|view| [(view.clone(), false), (view, true)])
            {
                let admitted = if projected {
                    inventory.admit_projected_with_native_requirements(
                        &joined,
                        vec![Arc::new(root.clone())],
                        &projection,
                        &facts,
                    )
                } else {
                    inventory.admit_certified_with_demand(
                        &joined,
                        vec![Arc::new(root.clone())],
                        NativeArtifactDemand::ScopeInterfaces,
                        &receipt,
                    )
                };
                let result =
                    admitted.and_then(|view| view.select_admission_roots(vec![root.descriptor.id]));
                match result {
                    Ok(selected) => {
                        assert_eq!(selected.capture_graph_selection().native_groups, expected);
                        assert!(!selected.artifact_ids().contains(&extra.descriptor.id));
                        assert_eq!(selected.namespace().len(), projection.roles().len() + 1);
                        let cold = ArtifactInventory::default();
                        let reopened = cold
                            .recover_selection(
                                &cold.empty_view(),
                                selected.entries(),
                                &selected.capture_graph_selection(),
                                &selected.binding_dependencies(),
                            )
                            .unwrap();
                        assert_eq!(
                            reopened.capture_graph_selection(),
                            selected.capture_graph_selection()
                        );
                        observations.push(true);
                    }
                    Err(error) => {
                        eprintln!(
                            "native_closure hidden_interfaces={hidden_interfaces} projected={projected}: {error:?}"
                        );
                        observations.push(false);
                    }
                }
            }
        }
        assert_eq!(observations, vec![true; 8]);
    }

    #[test]
    fn published_group_keeps_its_exact_artifact_child_over_equivalent_custody() {
        use crate::declaration_context::ExactDeclarationContext;
        use crate::declaration_join::ExactLexicalNode;

        let inventory = ArtifactInventory::default();
        let (native, values, _) = native_scope_fixture();
        let target = entry_in_unit("fixture", "Target", vec![]);
        let original = inventory
            .admit_shared_with_demand(
                &inventory.empty_view(),
                vec![
                    Arc::new(native.clone()),
                    Arc::new(values.clone()),
                    Arc::new(target.clone()),
                ],
                NativeArtifactDemand::AllGroups,
            )
            .unwrap();
        let sibling = inventory
            .admit_shared_with_demand(
                &inventory.empty_view(),
                vec![
                    Arc::new(values.clone()),
                    Arc::new(entry_in_unit("fixture", "Unrelated", vec![])),
                ],
                NativeArtifactDemand::ScopeInterfaces,
            )
            .unwrap();
        let original_values = original.namespace()[&values.descriptor.id];
        let sibling_values = sibling.namespace()[&values.descriptor.id];
        assert_ne!(original_values, sibling_values);
        let projection = CompilerInputProjection::from_issued_entries(&[
            Arc::new(native.clone()),
            Arc::new(values.clone()),
            Arc::new(target.clone()),
        ])
        .unwrap();
        let lexical = vec![
            ExactLexicalNode {
                owner: target.descriptor.owner.clone(),
                imports: vec![native.descriptor.owner.clone()],
            },
            ExactLexicalNode {
                owner: native.descriptor.owner.clone(),
                imports: vec![values.descriptor.owner.clone()],
            },
            ExactLexicalNode {
                owner: values.descriptor.owner.clone(),
                imports: vec![],
            },
        ];
        let issued = ExactDeclarationContext::from_authenticated_execution(
            [2; 32],
            &original,
            projection,
            lexical.clone(),
            target.descriptor.owner.clone(),
            &lexical
                .iter()
                .map(|node| node.owner.clone())
                .collect::<Vec<_>>(),
        )
        .unwrap();
        let publication = issued
            .issue_published_source_original("revision", "input", &native.descriptor.owner)
            .unwrap();
        let published = publication.context();
        let expected = published.artifact_view().capture_graph_selection();
        let group = expected.native_groups[0];
        for joined in [
            original.merge(&sibling).unwrap(),
            sibling.merge(&original).unwrap(),
        ] {
            let narrowed = joined
                .with_compiler_projection(&CompilerInputProjection::default())
                .unwrap();
            let restored_inventory = ArtifactInventory::default();
            let restored = restored_inventory
                .recover_selection(
                    &restored_inventory.empty_view(),
                    joined.entries(),
                    &joined.capture_graph_selection(),
                    &joined.binding_dependencies(),
                )
                .unwrap();
            for parent in [joined, narrowed, restored] {
                let owning = parent.inventory().clone();
                let before_nodes = owning
                    .0
                    .lock()
                    .unwrap()
                    .indices
                    .keys()
                    .copied()
                    .collect::<BTreeSet<_>>();
                let before_plans = owning
                    .0
                    .lock()
                    .unwrap()
                    .selection_plans
                    .keys()
                    .copied()
                    .collect::<BTreeSet<_>>();
                let before_edges = parent.binding_dependencies();
                let validation = crate::recovery_artifacts::PackageInterfaceValidation::default();
                let receipt =
                    crate::certified_products::CertifiedSourceSelection::from_compiler_projection(
                        published.compiler_input_projection(),
                        &parent.metadata_snapshot(),
                        &validation.inventory,
                    )
                    .unwrap();
                receipt
                    .selected_authored_original_closure(
                        &parent,
                        &native.descriptor.owner.unit,
                        &native.descriptor.owner.module,
                    )
                    .unwrap();
                let state = owning.0.lock().unwrap();
                let after_nodes = state.indices.keys().copied().collect::<BTreeSet<_>>();
                let warm_plans = state.selection_plans.len();
                for (scope, proof) in &state.selection_plans {
                    if !before_plans.contains(scope) {
                        assert!(proof
                            .nodes
                            .iter()
                            .all(|node| { matches!(node, ArtifactBindingNode::Artifact(_)) }));
                    }
                }
                drop(state);
                // Restoring a narrowed compiler namespace may issue artifact rows;
                // it cannot replace or add an executable native-group binding.
                assert!(after_nodes
                    .difference(&before_nodes)
                    .all(|node| { matches!(node, InventoryNodeKey::Artifact(_)) }));
                assert_eq!(parent.binding_dependencies(), before_edges);
                assert_eq!(scoped_group_child(&parent, group).1, original_values);
                let compiler_values = parent.namespace().get(&values.descriptor.id).copied();
                let mut admitted = parent;
                for _ in 0..2 {
                    admitted = owning
                        .admit_projected_with_native_requirements(
                            &admitted,
                            vec![Arc::new(native.clone())],
                            published.compiler_input_projection(),
                            &[],
                        )
                        .unwrap();
                    // Compiler roles preserve their selected namespace independently
                    // of the historical exact child issued with the native group.
                    if let Some(binding) = compiler_values {
                        assert_eq!(admitted.namespace()[&values.descriptor.id], binding);
                    }
                    let selected = admitted
                        .select_admission_roots(vec![native.descriptor.id])
                        .unwrap();
                    assert_eq!(
                        selected.capture_graph_selection().native_groups,
                        vec![group]
                    );
                    assert_eq!(scoped_group_child(&selected, group).1, original_values);
                    assert_eq!(owning.0.lock().unwrap().selection_plans.len(), warm_plans);
                }
            }
        }
    }

    #[test]
    fn issued_selection_refuses_unknown_roots_and_namespace_outside_exact_closure() {
        let inventory = ArtifactInventory::default();
        let root = entry("Root", &["Child"]);
        let child = entry("Child", &[]);
        let ArtifactPayload::Canonical(interface) = &child.payload else {
            unreachable!()
        };
        let next = ArtifactEntry::canonical(crate::certified_products::fixture_module_core(
            interface,
            b"next namespace Child Core".to_vec(),
        ));
        let first = inventory
            .admit(&inventory.empty_view(), vec![root.clone(), child])
            .unwrap();
        let second = inventory
            .admit(&inventory.empty_view(), vec![root.clone(), next])
            .unwrap();
        let projection = CompilerInputProjection::from_issued_entries(&second.entries()).unwrap();
        let joined = first.merge_selected(&second, &projection).unwrap();
        let issued = ExactArtifactSelection::capture(&first);
        assert_eq!(
            joined
                .select_issued(vec![root.descriptor.id], &issued)
                .unwrap()
                .capture_graph_selection(),
            first.capture_graph_selection(),
        );
        let alternate = second
            .capture_graph_selection()
            .namespace
            .into_iter()
            .find(|binding| binding.artifact == root.descriptor.id)
            .unwrap();
        let nodes_before = inventory.node_count();
        for mutation in 0..4 {
            let mut graph = first.capture_graph_selection();
            match mutation {
                0 => graph
                    .roots
                    .push(ArtifactBindingNode::Artifact(ArtifactBinding {
                        artifact: root.descriptor.id,
                        selection: ArtifactSelectionId([0xee; 32]),
                    })),
                1 => {
                    *graph
                        .namespace
                        .iter_mut()
                        .find(|binding| binding.artifact == root.descriptor.id)
                        .unwrap() = alternate;
                }
                2 => graph.namespace.push(graph.namespace[0]),
                _ => graph.bindings.push(graph.bindings[0]),
            }
            let transported = ExactArtifactSelection::from_recovered_graph_selection(graph);
            assert!(joined
                .select_issued(vec![root.descriptor.id], &transported)
                .is_err());
            assert_eq!(inventory.node_count(), nodes_before);
            assert_eq!(
                joined
                    .select_issued(vec![root.descriptor.id], &issued)
                    .unwrap()
                    .capture_graph_selection(),
                first.capture_graph_selection(),
            );
        }
    }

    #[derive(Clone, Debug)]
    struct NativeHistoryModel {
        roots: BTreeSet<ArtifactBindingNode>,
        rows: BTreeSet<(ArtifactBindingNode, ArtifactBindingNode, ArtifactDependency)>,
    }

    impl NativeHistoryModel {
        // The fixture authors one carrier and a group-7 binding to generation 4.
        // Only opaque handle correspondence comes from issuance; roots and edges
        // are defined here before observing the implementation's graph.
        fn authored(
            carrier: ArtifactBinding,
            canonical: ArtifactBinding,
            values: ArtifactBinding,
            group: NativeGroupBinding,
        ) -> Self {
            let carrier = ArtifactBindingNode::Artifact(carrier);
            let values = ArtifactBindingNode::Artifact(values);
            let group = ArtifactBindingNode::Group(group);
            Self {
                roots: BTreeSet::from([carrier, values, group]),
                rows: BTreeSet::from([
                    (
                        carrier,
                        ArtifactBindingNode::Artifact(canonical),
                        ArtifactDependency::Interface,
                    ),
                    (group, carrier, ArtifactDependency::Interface),
                    (
                        group,
                        values,
                        ArtifactDependency::NativeBinding {
                            dependent_ordinal: 7,
                            generation: 4,
                            namespace: "value".into(),
                            occurrence: "value".into(),
                            record_parent: None,
                        },
                    ),
                ]),
            }
        }

        fn nodes(&self) -> BTreeSet<ArtifactBindingNode> {
            let mut reached = self.roots.clone();
            loop {
                let next = self
                    .rows
                    .iter()
                    .filter(|(source, _, _)| reached.contains(source))
                    .map(|(_, target, _)| *target)
                    .collect::<Vec<_>>();
                let before = reached.len();
                reached.extend(next);
                if reached.len() == before {
                    return reached;
                }
            }
        }

        fn check(&self, view: &ArtifactView) -> proptest::test_runner::TestCaseResult {
            let graph = view.capture_graph_selection();
            let actual_nodes = graph
                .bindings
                .iter()
                .copied()
                .map(ArtifactBindingNode::Artifact)
                .chain(
                    graph
                        .native_groups
                        .iter()
                        .copied()
                        .map(ArtifactBindingNode::Group),
                )
                .collect::<BTreeSet<_>>();
            proptest::prop_assert_eq!(
                graph.bindings.len() + graph.native_groups.len(),
                actual_nodes.len()
            );
            proptest::prop_assert_eq!(actual_nodes, self.nodes(), "authored retained nodes");
            proptest::prop_assert_eq!(
                view.roots().iter().copied().collect::<BTreeSet<_>>(),
                self.roots.clone(),
                "authored live roots"
            );
            proptest::prop_assert!(
                view.custody_roots().is_empty(),
                "fixture has no separate live byte custody"
            );
            proptest::prop_assert_eq!(graph.roots.len(), self.roots.len(), "authored root count");
            proptest::prop_assert_eq!(
                graph.roots.into_iter().collect::<BTreeSet<_>>(),
                self.roots.clone(),
                "authored exact roots"
            );
            proptest::prop_assert_eq!(
                view.binding_dependencies()
                    .into_iter()
                    .collect::<BTreeSet<_>>(),
                self.rows.clone(),
                "authored exact edges"
            );
            proptest::prop_assert!(
                graph.custody_roots.is_empty(),
                "fixture has no separate byte-only custody"
            );
            Ok(())
        }
    }

    #[derive(Clone, Copy, Debug)]
    enum NativeHistoryOp {
        Narrow,
        Readmit,
        MergeOriginal,
        Recover,
        Add(u8, u8),
        Retire(u8),
        RefuseIncompleteEdges,
        RemoveAbsent,
    }

    fn native_history_op() -> impl proptest::strategy::Strategy<Value = NativeHistoryOp> {
        use proptest::strategy::Strategy;
        (0u8..8, 0u8..2, 0u8..3).prop_map(|(operation, owner, variant)| match operation {
            0 => NativeHistoryOp::Narrow,
            1 => NativeHistoryOp::Readmit,
            2 => NativeHistoryOp::MergeOriginal,
            3 => NativeHistoryOp::Recover,
            4 => NativeHistoryOp::Add(owner, variant),
            5 => NativeHistoryOp::Retire(owner),
            6 => NativeHistoryOp::RefuseIncompleteEdges,
            _ => NativeHistoryOp::RemoveAbsent,
        })
    }

    #[derive(Debug, Default)]
    struct NativeHistoryCoverage {
        operations: [usize; 8],
        added_bindings: usize,
        effective_retirements: usize,
        retired_bindings: usize,
        absent_retirements: usize,
        incomplete_edge_refusals: usize,
        peak_nodes: usize,
    }

    impl Drop for NativeHistoryCoverage {
        fn drop(&mut self) {
            eprintln!("exact_native_history coverage={self:?}");
        }
    }

    proptest::proptest! {
        #![proptest_config(proptest::test_runner::Config { cases: 64, ..Default::default() })]

        #[test]
        fn exact_native_binding_histories_preserve_typed_roots(
            generated in proptest::collection::vec(native_history_op(), 0..18),
        ) {
            let mut coverage = NativeHistoryCoverage::default();
            let inventory = ArtifactInventory::default();
            let (native, values, next) = native_scope_fixture();
            let old_values_id = values.descriptor.id;
            let new_child_id = next.descriptor.id;
            let ArtifactPayload::Original(product) = &native.payload else { unreachable!() };
            let canonical_id = ArtifactEntry::canonical(product.module_interface().unwrap().clone()).descriptor.id;
            proptest::prop_assert_ne!(old_values_id, new_child_id);
            let old = inventory.admit(&inventory.empty_view(), vec![native.clone(), values]).unwrap();
            let old_graph = old.capture_graph_selection();
            proptest::prop_assert_eq!(old_graph.native_groups.len(), 1);
            let old_group = old_graph.native_groups[0];
            proptest::prop_assert_eq!(old_group.original_ordinal, 7);
            let carrier = old.namespace()[&native.descriptor.id];
            let canonical = old.namespace()[&canonical_id];
            let old_values = old.namespace()[&old_values_id];
            proptest::prop_assert_eq!(carrier.artifact, native.descriptor.id);
            proptest::prop_assert_eq!(canonical.artifact, canonical_id);
            proptest::prop_assert_eq!(old_values.artifact, old_values_id);
            proptest::prop_assert_eq!(old_group.binding.artifact, native.descriptor.id);
            let mut original_model = NativeHistoryModel::authored(carrier, canonical, old_values, old_group);
            original_model.roots.insert(ArtifactBindingNode::Artifact(canonical));
            original_model.check(&old)?;
            let original_facts = ExactArtifactSelection::capture(&old);
            let projection = CompilerInputProjection::from_issued_entries(&[
                Arc::new(native.clone()), Arc::new(next.clone()),
            ]).unwrap();
            let mut view = inventory.admit_shared_with_intent(&old,
                vec![Arc::new(native.clone()), Arc::new(next)], NativeArtifactDemand::AllGroups,
                ArtifactRootIntent::Projected(&projection, &[])).unwrap();
            let current_graph = view.capture_graph_selection();
            proptest::prop_assert_eq!(current_graph.native_groups.len(), 2);
            let new_group = *current_graph.native_groups.iter().find(|group| **group != old_group).unwrap();
            proptest::prop_assert_eq!(new_group.original_ordinal, 7);
            proptest::prop_assert_eq!(new_group.binding.artifact, native.descriptor.id);
            proptest::prop_assert_ne!(new_group.binding.selection, carrier.selection);
            proptest::prop_assert_eq!(view.namespace()[&native.descriptor.id], carrier);
            let mut model = original_model.clone();
            let new_values = view.namespace()[&new_child_id];
            proptest::prop_assert_eq!(new_values.artifact, new_child_id);
            let new_facts = NativeHistoryModel::authored(carrier, canonical, new_values, new_group);
            model.roots.extend(new_facts.roots);
            model.rows.extend(new_facts.rows);
            model.check(&view)?;

            // Separate payload variants share an owner and interface while their
            // authored Core bytes differ. They never acquire a native group.
            let extras = (0..2).map(|owner| {
                let base = entry(&format!("Generated{owner}"), &[]);
                let ArtifactPayload::Canonical(interface) = base.payload else { unreachable!() };
                (0..3).map(|variant| ArtifactEntry::canonical(crate::certified_products::fixture_module_core(
                    &interface, format!("generated {owner} variant {variant}").into_bytes())))
                    .collect::<Vec<_>>()
            }).collect::<Vec<_>>();
            let extra_ids = extras.iter().flatten().map(|entry| entry.descriptor.id).collect::<BTreeSet<_>>();
            proptest::prop_assert_eq!(extra_ids.len(), 6);
            let mut operations = generated;
            let middle = operations.len() / 2;
            // Guided interacting history survives shrinking and forces two real
            // retirements, multiple payload variants, refusal and cold recovery.
            operations.splice(middle..middle, [NativeHistoryOp::Add(0, 0), NativeHistoryOp::Add(0, 1),
                NativeHistoryOp::Add(1, 2), NativeHistoryOp::Retire(0),
                NativeHistoryOp::RefuseIncompleteEdges, NativeHistoryOp::Recover, NativeHistoryOp::Retire(1)]);
            for operation in &operations {
                let index = match operation {
                    NativeHistoryOp::Narrow => 0, NativeHistoryOp::Readmit => 1,
                    NativeHistoryOp::MergeOriginal => 2, NativeHistoryOp::Recover => 3,
                    NativeHistoryOp::Add(..) => 4, NativeHistoryOp::Retire(_) => 5,
                    NativeHistoryOp::RefuseIncompleteEdges => 6, NativeHistoryOp::RemoveAbsent => 7,
                };
                coverage.operations[index] += 1;
                view = match *operation {
                    NativeHistoryOp::Narrow => view.with_compiler_projection(&projection.interface_only()).unwrap(),
                    NativeHistoryOp::Readmit => view.inventory().admit_shared_with_intent(&view,
                        vec![Arc::new(native.clone())], NativeArtifactDemand::AllGroups,
                        ArtifactRootIntent::Projected(&projection, &[])).unwrap(),
                    NativeHistoryOp::MergeOriginal => view.merge_selected(&old, &projection).unwrap(),
                    NativeHistoryOp::Recover => {
                        let restored = ArtifactInventory::default();
                        restored.recover_selection(&restored.empty_view(), view.entries(),
                            &view.capture_graph_selection(), &view.binding_dependencies()).unwrap()
                    },
                    NativeHistoryOp::Add(owner, variant) => {
                        let extra = extras[usize::from(owner)][usize::from(variant)].clone();
                        let independent = ArtifactInventory::default();
                        let packet = independent.admit(&independent.empty_view(), vec![extra.clone()]).unwrap();
                        let binding = packet.namespace()[&extra.descriptor.id];
                        proptest::prop_assert_eq!(binding.artifact, extra.descriptor.id);
                        let packet_model = NativeHistoryModel {
                            roots: BTreeSet::from([ArtifactBindingNode::Artifact(binding)]), rows: BTreeSet::new(),
                        };
                        packet_model.check(&packet)?;
                        coverage.added_bindings += usize::from(model.roots.insert(ArtifactBindingNode::Artifact(binding)));
                        view.merge_selected(&packet, &projection).unwrap()
                    },
                    NativeHistoryOp::Retire(owner) => {
                        let ids = extras[usize::from(owner)].iter().map(|entry| entry.descriptor.id).collect::<BTreeSet<_>>();
                        let before = model.roots.len();
                        model.roots.retain(|node| match node {
                            ArtifactBindingNode::Artifact(binding) => !ids.contains(&binding.artifact),
                            ArtifactBindingNode::Group(_) => true,
                        });
                        let removed = before - model.roots.len();
                        coverage.effective_retirements += usize::from(removed > 0);
                        coverage.absent_retirements += usize::from(removed == 0);
                        coverage.retired_bindings += removed;
                        view.support_without_owner(&extras[usize::from(owner)][0].descriptor.owner).unwrap()
                    },
                    NativeHistoryOp::RefuseIncompleteEdges => {
                        let refused = ArtifactInventory::default();
                        let mut incomplete = model.rows.iter().cloned().collect::<Vec<_>>();
                        incomplete.remove(0);
                        proptest::prop_assert!(refused.recover_selection(&refused.empty_view(), view.entries(),
                            &view.capture_graph_selection(), &incomplete).is_err());
                        proptest::prop_assert_eq!(refused.node_count(), 0, "refused recovery published no graph");
                        coverage.incomplete_edge_refusals += 1;
                        view
                    },
                    NativeHistoryOp::RemoveAbsent => view.support_without_owner(&module("Absent")).unwrap(),
                };
                coverage.peak_nodes = coverage.peak_nodes.max(model.nodes().len());
                model.check(&view).map_err(|error| proptest::test_runner::TestCaseError::fail(
                    format!("history={operations:?}; {error}")))?;
                let original = view.select_issued(vec![native.descriptor.id], &original_facts).unwrap();
                original_model.check(&original)?;
                // Root-only demand retains the old child as hidden support.
                let hidden = original.select_roots(vec![native.descriptor.id]).unwrap();
                let mut selected_model = original_model.clone();
                selected_model.roots.remove(&ArtifactBindingNode::Artifact(old_values));
                selected_model.roots.remove(&ArtifactBindingNode::Artifact(canonical));
                selected_model.check(&hidden)?;
                let issued = view.inventory().admit_shared_with_intent(&view, vec![Arc::new(native.clone())],
                    NativeArtifactDemand::AllGroups, ArtifactRootIntent::Projected(&projection, &[])).unwrap();
                let selected = issued.select_admission_roots(vec![native.descriptor.id]).unwrap();
                let mut selected_model = NativeHistoryModel::authored(carrier, canonical, new_values, new_group);
                selected_model.roots.remove(&ArtifactBindingNode::Artifact(new_values));
                selected_model.check(&selected)?;
                let requirements = selected.native_requirements_from_roots(&[
                    NativeRequirementRoot::Group { artifact: native.descriptor.id, original_ordinal: 7 },
                ]).unwrap();
                proptest::prop_assert_eq!(requirements.bindings.len(), 1);
                proptest::prop_assert_eq!(requirements.bindings[0].artifact_id, new_child_id);
            }
            proptest::prop_assert!(coverage.effective_retirements >= 2 && coverage.retired_bindings >= 3);
            proptest::prop_assert!(coverage.incomplete_edge_refusals >= 1);
        }
    }

    #[derive(Clone, Debug)]
    struct ExactNativeClosureModel {
        groups: BTreeSet<NativeGroupBinding>,
        rows: BTreeSet<(ArtifactBindingNode, ArtifactBindingNode, ArtifactDependency)>,
    }

    impl ExactNativeClosureModel {
        fn observe(
            &self,
            groups: &[NativeGroupBinding],
            rows: &[(ArtifactBindingNode, ArtifactBindingNode, ArtifactDependency)],
        ) -> Result<(), String> {
            if groups.len() != self.groups.len()
                || groups.iter().copied().collect::<BTreeSet<_>>() != self.groups
            {
                return Err("native group choices differ from the independent issuers".into());
            }
            let closure =
                |rows: &[(ArtifactBindingNode, ArtifactBindingNode, ArtifactDependency)]| {
                    let mut reached = self
                        .groups
                        .iter()
                        .copied()
                        .map(ArtifactBindingNode::Group)
                        .collect::<BTreeSet<_>>();
                    loop {
                        let next = rows
                            .iter()
                            .filter(|(from, _, _)| reached.contains(from))
                            .map(|(_, to, _)| *to)
                            .collect::<Vec<_>>();
                        let before = reached.len();
                        reached.extend(next);
                        if before == reached.len() {
                            return (
                                reached.clone(),
                                rows.iter()
                                    .filter(|(from, _, _)| reached.contains(from))
                                    .cloned()
                                    .collect::<BTreeSet<_>>(),
                            );
                        }
                    }
                };
            let expected = closure(&self.rows.iter().cloned().collect::<Vec<_>>());
            let actual = closure(rows);
            if actual != expected {
                return Err(format!(
                    "exact native closure changed: expected={expected:?}, actual={actual:?}"
                ));
            }
            Ok(())
        }

        fn check(&self, view: &ArtifactView) -> Result<(), String> {
            self.observe(
                &view.capture_graph_selection().native_groups,
                &view.binding_dependencies(),
            )
        }
    }

    struct IndependentNativeClosures {
        originals: [Arc<ArtifactEntry>; 2],
        sources: [ArtifactView; 2],
        requirements: [NativeOriginalRequirement; 2],
        interfaces: CompilerInputProjection,
        model: ExactNativeClosureModel,
    }

    impl IndependentNativeClosures {
        fn new(depth: usize, groups: u32) -> Self {
            let shared = (0..depth)
                .map(|index| {
                    let name = format!("Shared{index}");
                    let previous = format!("Shared{}", index.saturating_sub(1));
                    let dependencies = [previous.as_str()];
                    entry(&name, if index == 0 { &[] } else { &dependencies })
                })
                .map(Arc::new)
                .collect::<Vec<_>>();
            let originals = ["NativeLeft", "NativeRight"].map(|name| {
                Arc::new(issued_native_groups(
                    name,
                    (7..7 + groups).map(|ordinal| (ordinal, vec![])).collect(),
                    &[shared.last().unwrap()],
                    &BTreeMap::new(),
                ))
            });
            let sources = originals.each_ref().map(|original| {
                let inventory = ArtifactInventory::default();
                let entries = std::iter::once(Arc::clone(original))
                    .chain(shared.iter().cloned())
                    .collect();
                inventory
                    .admit_shared(&inventory.empty_view(), entries)
                    .unwrap()
            });
            let mut model = ExactNativeClosureModel {
                groups: BTreeSet::new(),
                rows: BTreeSet::new(),
            };
            for (original, source) in originals.iter().zip(&sources) {
                let namespace = source.namespace();
                let ArtifactPayload::Original(product) = &original.payload else {
                    unreachable!()
                };
                let canonical =
                    ArtifactEntry::canonical(product.module_interface().unwrap().clone());
                let carrier = ArtifactBindingNode::Artifact(namespace[&original.descriptor.id]);
                let canonical = ArtifactBindingNode::Artifact(namespace[&canonical.descriptor.id]);
                model
                    .rows
                    .insert((carrier, canonical, ArtifactDependency::Interface));
                model.rows.insert((
                    canonical,
                    ArtifactBindingNode::Artifact(namespace[&shared.last().unwrap().descriptor.id]),
                    ArtifactDependency::Interface,
                ));
                for pair in shared.windows(2) {
                    model.rows.insert((
                        ArtifactBindingNode::Artifact(namespace[&pair[1].descriptor.id]),
                        ArtifactBindingNode::Artifact(namespace[&pair[0].descriptor.id]),
                        ArtifactDependency::Interface,
                    ));
                }
                let issued = source.capture_graph_selection().native_groups;
                assert_eq!(issued.len(), groups as usize);
                assert_eq!(
                    issued
                        .iter()
                        .map(|group| (group.binding.artifact, group.original_ordinal))
                        .collect::<BTreeSet<_>>(),
                    (7..7 + groups)
                        .map(|ordinal| (original.descriptor.id, ordinal))
                        .collect()
                );
                for group in issued {
                    assert!(model.groups.insert(group));
                    model.rows.insert((
                        ArtifactBindingNode::Group(group),
                        carrier,
                        ArtifactDependency::Interface,
                    ));
                }
            }
            for shared in &shared {
                assert_ne!(
                    sources[0].namespace()[&shared.descriptor.id],
                    sources[1].namespace()[&shared.descriptor.id]
                );
            }
            let requirements = std::array::from_fn(|index| {
                NativeOriginalRequirement::from_selected_view(
                    originals[index].descriptor.id,
                    &sources[index],
                )
                .unwrap()
                .unwrap()
            });
            let interfaces = CompilerInputProjection::from_issued_entries(
                &originals.iter().cloned().chain(shared).collect::<Vec<_>>(),
            )
            .unwrap()
            .interface_only();
            Self {
                originals,
                sources,
                requirements,
                interfaces,
                model,
            }
        }

        fn join(&self, reverse: bool) -> ArtifactView {
            let first = usize::from(reverse);
            self.sources[first]
                .merge_selected(&self.sources[1 - first], &self.interfaces)
                .unwrap()
        }

        fn promote(&self, view: &ArtifactView, mask: u8) -> Result<ArtifactView, CompileError> {
            let entries = self
                .originals
                .iter()
                .enumerate()
                .filter(|(index, _)| mask & (1 << index) != 0)
                .map(|(_, entry)| Arc::clone(entry))
                .collect::<Vec<_>>();
            let projection = self
                .interfaces
                .merge(&CompilerInputProjection::from_issued_entries(&entries)?)?;
            let requirements = self
                .requirements
                .iter()
                .enumerate()
                .filter(|(index, _)| mask & (1 << index) != 0)
                .map(|(_, fact)| fact.clone())
                .collect::<Vec<_>>();
            view.inventory().admit_projected_with_native_requirements(
                view,
                entries,
                &projection,
                &requirements,
            )
        }

        fn reject_equal_payload_substitution(&self, view: &ArtifactView) {
            let mut mutated = view.binding_dependencies();
            let shared = entry("Shared0", &[]).descriptor.id;
            let edge = mutated
                .iter_mut()
                .find(|(_, target, _)| {
                    *target == ArtifactBindingNode::Artifact(self.sources[0].namespace()[&shared])
                })
                .unwrap();
            edge.1 = ArtifactBindingNode::Artifact(self.sources[1].namespace()[&shared]);
            assert!(self
                .model
                .observe(&view.capture_graph_selection().native_groups, &mutated)
                .is_err());
        }

        fn join_later(&self, view: &ArtifactView, ordinal: usize) -> ArtifactView {
            let namespace_ids = view.namespace().keys().copied().collect::<BTreeSet<_>>();
            let entries = view
                .entries()
                .into_iter()
                .filter(|entry| namespace_ids.contains(&entry.descriptor.id))
                .collect::<Vec<_>>();
            let source_projection = CompilerInputProjection::from_issued_entries(&entries).unwrap();
            let name = format!("LateJoin{ordinal}");
            let canonical = crate::certified_products::fixture_module_interface(
                [2; 32],
                "unit",
                &name,
                BTreeMap::new(),
            );
            let interface = CertifiedJoinedInterface::from_certification(
                [2; 32],
                "unit".into(),
                name,
                canonical.interface_bytes().to_vec(),
                canonical.package_imports_bytes().to_vec(),
            )
            .unwrap();
            let joined = Arc::new(ArtifactEntry::interface(
                interface,
                JoinedInterfaceRole::LexicalJoin,
                entries
                    .iter()
                    .filter(|entry| !entry.is_native())
                    .map(|entry| entry.descriptor.owner.clone())
                    .collect(),
            ));
            let issued = CompilerInputProjection::from_issued_entries(&[joined.clone()]).unwrap();
            let namespace = source_projection.merge(&issued).unwrap();
            let admitted = view
                .inventory()
                .admit_projected_with_native_requirements(
                    view,
                    vec![joined],
                    &namespace.interface_only(),
                    &[],
                )
                .unwrap();
            admitted.merge_selected(view, &namespace).unwrap()
        }

        fn refuse_conflicting_choice(&self, view: &ArtifactView, empty: bool) {
            let inventory = view.inventory();
            let carriers = inventory
                .admit_shared_with_demand(
                    &inventory.empty_view(),
                    self.sources[0].entries(),
                    NativeArtifactDemand::ScopeInterfaces,
                )
                .unwrap();
            let first = inventory
                .admit_shared(&carriers, vec![self.originals[0].clone()])
                .unwrap();
            let second = inventory
                .admit_shared(
                    &carriers,
                    vec![
                        self.originals[0].clone(),
                        Arc::new(entry("AmbiguousExtra", &[])),
                    ],
                )
                .unwrap();
            let requirements = [&first, &second].map(|issuer| {
                NativeOriginalRequirement::from_selected_view(
                    self.originals[0].descriptor.id,
                    issuer,
                )
                .unwrap()
                .unwrap()
            });
            assert_ne!(requirements[0], requirements[1]);
            let ambiguous = view.merge(&first).unwrap().merge(&second).unwrap();
            let projection = self
                .interfaces
                .merge(
                    &CompilerInputProjection::from_issued_entries(&[self.originals[0].clone()])
                        .unwrap(),
                )
                .unwrap();
            let graph = ambiguous.capture_graph_selection();
            let nodes = inventory.node_count();
            assert!(
                matches!(inventory.admit_projected_with_native_requirements(&ambiguous,
                if empty { vec![] } else { vec![self.originals[0].clone()] }, &projection, &requirements),
                Err(CompileError::ArtifactInventory(error))
                    if matches!(error.failure, ArtifactInventoryFailure::BindingAmbiguity { artifact }
                        if artifact == self.originals[0].descriptor.id))
            );
            assert_eq!(inventory.node_count(), nodes);
            assert_eq!(ambiguous.capture_graph_selection(), graph);
        }
    }

    fn check_late_native_original_recapture(
        helper: Arc<ArtifactEntry>,
        interfaces: Vec<Arc<ArtifactEntry>>,
        helper_import: crate::certified_products::PendingImportOwner,
        producer_bytes: &[u8],
        products: &[crate::recovery_artifacts::CertifiedRecoveryProduct],
    ) {
        use crate::declaration_context::{ExactDeclarationContext, OriginalCompilerInputs};
        let producer = CanonicalProducerIdentity::from_producer_bytes(producer_bytes);
        let inventory = ArtifactInventory::default();
        let originals = ["RecaptureLeft", "RecaptureRight"].map(|name| {
            let raw = crate::certified_products::tests::original_groups_fixture_with_interface(
                name,
                vec![(7, vec![helper_import.clone()])],
                1,
                &BTreeMap::new(),
                name.as_bytes().to_vec(),
            );
            let raw = crate::certified_products::fixture_finalized_product_with_requirements(
                raw,
                producer.sha256(),
                Some(BTreeMap::from([(
                    (
                        helper.descriptor.owner.unit.clone(),
                        helper.descriptor.owner.module.clone(),
                    ),
                    helper.descriptor.interface_sha256,
                )])),
            );
            let product = crate::certified_products::tests::recovered_witness_fixtures(&[raw])
                .remove(0)
                .product;
            Arc::new(ArtifactEntry::original(producer.sha256(), product).unwrap())
        });
        let mut entries = interfaces;
        entries.extend([helper.clone(), originals[0].clone(), originals[1].clone()]);
        let available = inventory
            .admit_shared_with_demand(
                &inventory.empty_view(),
                entries.clone(),
                NativeArtifactDemand::ScopeInterfaces,
            )
            .unwrap();
        let receipt = bound_selection_for_entries(&available, &entries);
        let issuers = originals.each_ref().map(|original| {
            let mut fixed = inventory
                .bind_segment(&available, entries.clone(), &receipt)
                .unwrap();
            inventory
                .admit_bound_target(
                    &mut fixed,
                    &inventory.empty_view(),
                    NativeArtifactDemand::CertifiedTargetImports(&[issued_source(original, 7)]),
                    vec![],
                    &receipt,
                )
                .unwrap()
        });
        let expected = ExactNativeClosureModel {
            groups: issuers
                .iter()
                .flat_map(|view| view.capture_graph_selection().native_groups)
                .collect(),
            rows: issuers
                .iter()
                .flat_map(ArtifactView::binding_dependencies)
                .collect(),
        };
        let helper_groups = expected
            .groups
            .iter()
            .filter(|group| group.binding.artifact == helper.descriptor.id)
            .copied()
            .collect::<Vec<_>>();
        assert_eq!(helper_groups.len(), 2);
        assert_ne!(
            helper_groups[0].binding.selection,
            helper_groups[1].binding.selection
        );
        let carriers = expected
            .rows
            .iter()
            .filter_map(|(from, to, edge)| match (from, to, edge) {
                (
                    ArtifactBindingNode::Group(group),
                    ArtifactBindingNode::Artifact(carrier),
                    ArtifactDependency::Interface,
                ) if group.binding.artifact == helper.descriptor.id
                    && carrier.artifact == helper.descriptor.id =>
                {
                    Some(*carrier)
                }
                _ => None,
            })
            .collect::<BTreeSet<_>>();
        assert_eq!(
            carriers.len(),
            1,
            "distinct group issuers retain the same exact carrier"
        );
        let projection = CompilerInputProjection::from_issued_entries(&entries).unwrap();
        let publications = issuers
            .iter()
            .enumerate()
            .map(|(index, issuer)| {
                ExactDeclarationContext::from_authenticated_execution(
                    producer.sha256(),
                    issuer,
                    projection.clone(),
                    vec![
                        crate::declaration_join::ExactLexicalNode {
                            owner: originals[index].descriptor.owner.clone(),
                            imports: vec![helper.descriptor.owner.clone()],
                        },
                        crate::declaration_join::ExactLexicalNode {
                            owner: helper.descriptor.owner.clone(),
                            imports: vec![],
                        },
                    ],
                    originals[index].descriptor.owner.clone(),
                    &[
                        originals[index].descriptor.owner.clone(),
                        helper.descriptor.owner.clone(),
                    ],
                )
                .unwrap()
                .issue_published_source_original("issuer", "input", &helper.descriptor.owner)
                .unwrap()
            })
            .collect::<Vec<_>>();
        for publication in &publications {
            let imports = [issued_source(&helper, 7)];
            let groups = publication
                .context()
                .artifact_view()
                .native_groups_for_target(None, &imports)
                .unwrap();
            assert!(!groups.is_empty());
            assert_eq!(
                publication
                    .context()
                    .published_source_original_selections_for_target(&groups, &imports,)
                    .unwrap()
                    .len(),
                1,
                "an authenticated source root retains its issued compilation policy",
            );
            assert!(
                publication
                    .context()
                    .published_source_original_selections_for_target(&BTreeSet::new(), &[])
                    .unwrap()
                    .is_empty(),
                "an unrelated publication is not selected by ambient availability",
            );
        }
        for (index, issuer) in issuers.iter().enumerate() {
            let actual = issuer
                .native_groups_for_target(None, &[issued_source(&originals[index], 7)])
                .unwrap();
            let helper_group = *actual
                .iter()
                .find(|group| group.binding.artifact == helper.descriptor.id)
                .unwrap();
            assert!(issuer.contains_issued_native_group(helper_group));
            assert_eq!(
                publications[index]
                    .context()
                    .published_source_original_selections_for_target(&actual, &[])
                    .unwrap()
                    .len(),
                1
            );
            assert!(publications[1 - index]
                .context()
                .published_source_original_selections_for_target(&actual, &[])
                .unwrap()
                .is_empty());
            let mixed = actual
                .iter()
                .copied()
                .chain(helper_groups.iter().copied())
                .collect();
            assert!(publications[index]
                .context()
                .published_source_original_selections_for_target(&mixed, &[])
                .unwrap()
                .is_empty());

            assert!(!issuers[1 - index].contains_issued_native_group(helper_group));
            let narrowed = issuer
                .select_publication_roots(
                    originals[index].descriptor.id,
                    vec![originals[index].descriptor.id],
                )
                .unwrap();
            assert!(narrowed.contains_issued_native_group(helper_group));
            assert!(!narrowed.contains_issued_native_group(
                *helper_groups
                    .iter()
                    .find(|other| **other != helper_group)
                    .unwrap()
            ));
        }
        for reverse in [false, true] {
            let first = usize::from(reverse);
            let joined = issuers[first]
                .merge_selected(&issuers[1 - first], &projection)
                .unwrap();
            assert!(joined.lease.roots.is_empty());
            assert!(joined.roots().iter().any(|root| matches!(root, InventoryNodeKey::Group(group) if group.binding.artifact == originals[0].descriptor.id)));
            assert!(!joined.roots().iter().any(|root| matches!(root, InventoryNodeKey::Group(group) if group.binding.artifact == helper.descriptor.id)));
            // Reproduce the old retention rule as a control: exact descendants
            // become roots and private compiler input recapture must refuse.
            let legacy_roots = joined
                .capture_graph_selection()
                .native_groups
                .into_iter()
                .map(InventoryNodeKey::Group)
                .chain(joined.roots().iter().copied())
                .collect();
            let mut legacy = joined
                .retain_projected_materializations(inventory.retain(
                    legacy_roots,
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                ))
                .unwrap();
            legacy.replace_namespace(&joined.namespace());
            let legacy_context = ExactDeclarationContext::from_authenticated_execution(
                producer.sha256(),
                &legacy,
                projection.clone(),
                vec![],
                originals[0].descriptor.owner.clone(),
                &[],
            )
            .unwrap();
            assert!(
                matches!(OriginalCompilerInputs::from_native_availability(&legacy_context, producer, products),
                Err(CompileError::ArtifactInventory(error)) if matches!(error.failure,
                    ArtifactInventoryFailure::BindingAmbiguity { artifact } if artifact == helper.descriptor.id))
            );

            let retained = joined.retain_original_custody(&joined).unwrap();
            expected
                .check(&retained)
                .expect("retention preserves issuing groups");
            assert_eq!(retained.roots(), joined.roots());
            assert!(helper_groups.iter().all(|group| {
                retained
                    .custody_roots()
                    .contains(&InventoryNodeKey::Group(*group))
            }));
            let generated = Arc::new(entry("RecaptureScaffold", &[]));
            let scaffold = inventory
                .admit_shared(&inventory.empty_view(), vec![generated.clone()])
                .unwrap();
            let with_scaffold = retained.merge_selected(&scaffold, &projection).unwrap();
            let support = with_scaffold
                .support_without_owner(&generated.descriptor.owner)
                .unwrap();
            expected
                .check(&support)
                .expect("support preserves issuing groups");
            assert!(!support.roots().iter().any(|root| matches!(root, InventoryNodeKey::Group(group) if group.binding.artifact == helper.descriptor.id)));
            let recovery = ArtifactInventory::default();
            let restored = recovery
                .recover_selection(
                    &recovery.empty_view(),
                    support.entries(),
                    &support.capture_graph_selection(),
                    &support.binding_dependencies(),
                )
                .unwrap();
            for view in [&joined, &retained, &support, &restored] {
                let context = ExactDeclarationContext::from_authenticated_execution(
                    producer.sha256(),
                    view,
                    projection.clone(),
                    vec![],
                    originals[0].descriptor.owner.clone(),
                    &[],
                )
                .unwrap();
                let inputs =
                    OriginalCompilerInputs::from_native_availability(&context, producer, products)
                        .unwrap();
                let scratch = tempfile::tempdir().unwrap();
                let request =
                    crate::declaration_context::ExactCompileContext::new(Arc::new(context))
                        .prepare_compilation_with_private_input(
                            scratch.path(),
                            producer_bytes,
                            None,
                            Some(inputs),
                        )
                        .unwrap();
                let facts = &request.compiler_inputs().native_original_requirements;
                assert!(!facts
                    .iter()
                    .any(|fact| fact.original == helper.descriptor.id));
                for original in &originals {
                    assert!(facts
                        .iter()
                        .any(|fact| fact.original == original.descriptor.id));
                }
                let admitted = view
                    .inventory()
                    .admit_projected_with_native_requirements(
                        view,
                        originals.to_vec(),
                        &projection,
                        facts,
                    )
                    .unwrap();
                expected.check(&admitted).unwrap();
                let graph = admitted.capture_graph_selection();
                let count = admitted.inventory().node_count();
                assert!(
                    matches!(admitted.native_requirements_from_roots(&[NativeRequirementRoot::Group {
                    artifact: helper.descriptor.id, original_ordinal: helper_groups[0].original_ordinal,
                }]), Err(CompileError::ArtifactInventory(error)) if matches!(error.failure,
                    ArtifactInventoryFailure::BindingAmbiguity { artifact } if artifact == helper.descriptor.id))
                );
                assert_eq!(admitted.capture_graph_selection(), graph);
                assert_eq!(admitted.inventory().node_count(), count);
            }
        }
    }

    #[test]
    fn support_exclusion_refuses_reintroduced_owner_and_old_custody_transport() {
        let inventory = ArtifactInventory::default();
        let excluded = entry("ExcludedSupport", &[]);
        let dependent = entry("DependentSupport", &["ExcludedSupport"]);
        let view = inventory
            .admit(&inventory.empty_view(), vec![dependent, excluded.clone()])
            .unwrap();
        let before = view.capture_graph_selection();
        let nodes = inventory.node_count();
        assert!(view
            .support_without_owner(&excluded.descriptor.owner)
            .is_err());
        assert_eq!(view.capture_graph_selection(), before);
        assert_eq!(inventory.node_count(), nodes);
        let mut old = serde_json::to_value(&before).unwrap();
        let object = old.as_object_mut().unwrap();
        let custody = object.remove("custody_roots").unwrap();
        object.insert("native_custody".into(), custody);
        assert!(serde_json::from_value::<ArtifactGraphSelection>(old).is_err());
    }

    #[test]
    fn late_native_original_recapture_keeps_inherited_roots_and_historical_children() {
        let producer = CanonicalProducerIdentity::from_producer_bytes(b"native recapture fixture");
        let product = crate::certified_products::tests::original_groups_fixture(
            "RecaptureHelper",
            vec![(7, Vec::new())],
            1,
            &BTreeMap::new(),
        );
        let product =
            crate::certified_products::fixture_finalized_product(product, producer.sha256());
        let product = crate::certified_products::tests::recovered_witness_fixtures(&[product])
            .remove(0)
            .product;
        let helper = Arc::new(ArtifactEntry::original(producer.sha256(), product.clone()).unwrap());
        check_late_native_original_recapture(
            helper.clone(),
            vec![],
            issued_source(&helper, 7),
            b"native recapture fixture",
            &[product],
        );
    }

    #[test]
    #[ignore = "requires declared compiler deployment and native catalog"]
    fn borrowed_native_original_late_recapture_preserves_issued_descendants() {
        let configuration = crate::toolchain::CompilerDeploymentConfiguration::from_env().unwrap();
        let (endpoint, _, _) =
            crate::toolchain::bind_admitted_extract_endpoint(&configuration).unwrap();
        let package = crate::toolchain::CatalogSelection::FreshConfigured
            .acquire()
            .unwrap()
            .expect("declared native catalog");
        let scratch = tempfile::tempdir().unwrap();
        let candidates = crate::module_candidates::select_acquired_catalog(
            &package,
            endpoint.identity().producer_bytes(),
            &package.source_selection().include_roots(),
            scratch.path(),
        )
        .unwrap()
        .expect("catalog candidates");
        let canonical = candidates
            .by_owner
            .iter()
            .map(|(owner, candidate)| (owner.clone(), candidate.original_module_interface.clone()))
            .collect::<BTreeMap<_, _>>();
        let protected = crate::module_candidates::ExactCandidateContext::new(
            canonical.keys().cloned().collect(),
            BTreeSet::new(),
        )
        .with_canonical_interfaces(canonical, BTreeSet::new());
        let private = crate::module_candidates::select_with_catalog(
            &crate::toolchain::CatalogSelection::Acquired(Some(package.clone())),
            endpoint.identity().producer_bytes(),
            &package.source_selection().include_roots(),
            scratch.path(),
            Some(&protected),
        )
        .unwrap()
        .expect("protected catalog native availability");
        let producer =
            CanonicalProducerIdentity::from_producer_bytes(endpoint.identity().producer_bytes());
        let product = private
            .native_availability
            .iter()
            .find(|product| product.owner().module == "Tidepool.View.Types")
            .expect("borrowed View.Types original")
            .clone();
        let helper = Arc::new(ArtifactEntry::original(producer.sha256(), product.clone()).unwrap());
        assert!(helper.catalog_input_custody().is_some());
        let interfaces = candidates
            .by_owner
            .values()
            .map(|candidate| {
                Arc::new(ArtifactEntry::canonical(
                    candidate.original_module_interface.clone(),
                ))
            })
            .collect();
        let binder = tidepool_repr::execution_schema::SymbolIdentity {
            unit: product.owner().unit.clone(),
            module: product.owner().module.clone(),
            namespace: "value".into(),
            occurrence: "viewTree".into(),
            record_parent: None,
        };
        let groups = crate::certified_products::original_available_groups(&product)
            .unwrap()
            .filter(|group| group.binders().contains(&binder))
            .collect::<Vec<_>>();
        assert_eq!(
            groups.len(),
            1,
            "one certified group owns View.Types.viewTree"
        );
        let helper_import = crate::certified_products::PendingImportOwner::Source {
            owner: product.owner().clone(),
            original_ordinal: groups[0].original_ordinal(),
            binder,
        };
        check_late_native_original_recapture(
            helper,
            interfaces,
            helper_import,
            endpoint.identity().producer_bytes(),
            &private.native_availability,
        );
    }
    #[test]
    fn independent_native_roots_keep_overlapping_exact_descendant_bindings() {
        for count in [1, 3] {
            let helper = Arc::new(issued_native_groups(
                "SharedNative",
                (7..7 + count).map(|ordinal| (ordinal, vec![])).collect(),
                &[],
                &BTreeMap::new(),
            ));
            let originals = ["NativeLeft", "NativeRight"].map(|name| {
                Arc::new(issued_native_groups(
                    name,
                    vec![(
                        7,
                        (7..7 + count)
                            .map(|ordinal| issued_source(&helper, ordinal))
                            .collect(),
                    )],
                    &[&helper],
                    &BTreeMap::new(),
                ))
            });
            let sources = originals.each_ref().map(|original| {
                let inventory = ArtifactInventory::default();
                inventory
                    .admit_shared(
                        &inventory.empty_view(),
                        vec![helper.clone(), original.clone()],
                    )
                    .unwrap()
            });
            let requirements = std::array::from_fn::<_, 2, _>(|index| {
                NativeOriginalRequirement::from_selected_view(
                    originals[index].descriptor.id,
                    &sources[index],
                )
                .unwrap()
                .unwrap()
            });
            let interfaces = CompilerInputProjection::from_issued_entries(&[
                helper.clone(),
                originals[0].clone(),
                originals[1].clone(),
            ])
            .unwrap()
            .interface_only();
            let projection = interfaces
                .merge(&CompilerInputProjection::from_issued_entries(&originals).unwrap())
                .unwrap();
            let model = ExactNativeClosureModel {
                groups: sources
                    .iter()
                    .flat_map(|view| view.capture_graph_selection().native_groups)
                    .collect(),
                rows: sources
                    .iter()
                    .flat_map(ArtifactView::binding_dependencies)
                    .collect(),
            };
            assert_eq!(
                model
                    .groups
                    .iter()
                    .filter(|group| group.binding.artifact == helper.descriptor.id)
                    .count(),
                2 * count as usize
            );
            for reverse in [false, true] {
                let first = usize::from(reverse);
                let joined = sources[first]
                    .merge_selected(&sources[1 - first], &interfaces)
                    .unwrap();
                let selected = joined
                    .inventory()
                    .admit_projected_with_native_requirements(
                        &joined,
                        originals.to_vec(),
                        &projection,
                        &requirements,
                    )
                    .unwrap();
                model.check(&selected).unwrap();
                let recovered_inventory = ArtifactInventory::default();
                let recovered = recovered_inventory
                    .recover_selection(
                        &recovered_inventory.empty_view(),
                        selected.entries(),
                        &selected.capture_graph_selection(),
                        &selected.binding_dependencies(),
                    )
                    .unwrap();
                model.check(&recovered).unwrap();
                let mut certified =
                    crate::certified_products::CertifiedSourceSelection::from_compiler_projection(
                        &projection,
                        &recovered.metadata_snapshot(),
                        &crate::recovery_artifacts::PackageInterfaceValidation::default().inventory,
                    )
                    .unwrap();
                certified
                    .with_native_original_requirements(
                        &recovered.metadata_snapshot(),
                        &requirements,
                    )
                    .unwrap();
                let mut bound = recovered_inventory
                    .bind_segment(&recovered, vec![], &certified)
                    .unwrap();
                let imports = originals
                    .each_ref()
                    .map(|original| issued_source(original, 7));
                let target = recovered_inventory
                    .admit_bound_target(
                        &mut bound,
                        &recovered,
                        NativeArtifactDemand::CertifiedTargetImports(&imports),
                        vec![],
                        &certified,
                    )
                    .unwrap();
                model.check(&target).unwrap();
                let helper_imports = [issued_source(&helper, 7)];
                let before_bound_graph = recovered.capture_graph_selection();
                let before_bound_nodes = recovered_inventory.node_count();
                assert!(matches!(recovered_inventory.admit_bound_target(
                    &mut bound, &recovered,
                    NativeArtifactDemand::CertifiedTargetImports(&helper_imports),
                    vec![], &certified,
                ), Err(CompileError::ArtifactInventory(error)) if matches!(error.failure,
                    ArtifactInventoryFailure::BindingAmbiguity { artifact } if artifact == helper.descriptor.id)));
                assert_eq!(recovered_inventory.node_count(), before_bound_nodes);
                assert_eq!(recovered.capture_graph_selection(), before_bound_graph);

                let helper_projection = projection
                    .merge(
                        &CompilerInputProjection::from_issued_entries(&[helper.clone()]).unwrap(),
                    )
                    .unwrap();
                let mut helper_requirements = requirements.to_vec();
                helper_requirements.push(
                    NativeOriginalRequirement::from_selected_view(
                        helper.descriptor.id,
                        &sources[first],
                    )
                    .unwrap()
                    .unwrap(),
                );
                let chosen = joined
                    .inventory()
                    .admit_projected_with_native_requirements(
                        &joined,
                        vec![helper.clone()],
                        &helper_projection,
                        &helper_requirements,
                    )
                    .unwrap();
                model.check(&chosen).unwrap();
                let mut chosen_certified =
                    crate::certified_products::CertifiedSourceSelection::from_compiler_projection(
                        &helper_projection,
                        &chosen.metadata_snapshot(),
                        &crate::recovery_artifacts::PackageInterfaceValidation::default().inventory,
                    )
                    .unwrap();
                chosen_certified
                    .with_native_original_requirements(
                        &chosen.metadata_snapshot(),
                        &helper_requirements,
                    )
                    .unwrap();
                let mut chosen_bound = chosen
                    .inventory()
                    .bind_segment(&chosen, vec![], &chosen_certified)
                    .unwrap();
                let direct = chosen
                    .inventory()
                    .admit_bound_target(
                        &mut chosen_bound,
                        &chosen,
                        NativeArtifactDemand::CertifiedTargetImports(&helper_imports),
                        vec![],
                        &chosen_certified,
                    )
                    .unwrap();
                model.check(&direct).unwrap();

                let fresh = Arc::new(issued_native_groups(
                    "FreshRoot",
                    vec![(7, vec![issued_source(&helper, 7)])],
                    &[&helper],
                    &BTreeMap::new(),
                ));
                let fresh_projection = projection
                    .merge(&CompilerInputProjection::from_issued_entries(&[fresh.clone()]).unwrap())
                    .unwrap();
                let before_graph = selected.capture_graph_selection();
                let before_nodes = selected.inventory().node_count();
                assert!(matches!(selected.inventory().admit_shared_with_intent(
                &selected, vec![fresh], NativeArtifactDemand::AllGroups,
                ArtifactRootIntent::Projected(&fresh_projection, &requirements),
            ), Err(CompileError::ArtifactInventory(error)) if matches!(error.failure,
                ArtifactInventoryFailure::BindingAmbiguity { artifact } if artifact == helper.descriptor.id)));
                assert_eq!(selected.inventory().node_count(), before_nodes);
                assert_eq!(selected.capture_graph_selection(), before_graph);
                let conflicts = sources.each_ref().map(|view| {
                    NativeOriginalRequirement::from_selected_view(helper.descriptor.id, view)
                        .unwrap()
                        .unwrap()
                });
                let helper_projection = interfaces
                    .merge(
                        &CompilerInputProjection::from_issued_entries(&[helper.clone()]).unwrap(),
                    )
                    .unwrap();
                assert!(
                    matches!(joined.inventory().admit_projected_with_native_requirements(
                &joined, vec![helper.clone()], &helper_projection, &conflicts,
            ), Err(CompileError::ArtifactInventory(error)) if matches!(error.failure,
                ArtifactInventoryFailure::BindingAmbiguity { artifact } if artifact == helper.descriptor.id))
                );
            }
        }
    }

    #[test]
    fn private_originals_keep_independent_bindings_of_shared_canonical_dependencies() {
        let fixture = IndependentNativeClosures::new(1, 1);
        for reverse in [false, true] {
            let joined = fixture.join(reverse);
            fixture.model.check(&joined).unwrap();
            for mask in [1, 2, 3] {
                let promoted = fixture
                    .promote(&joined, mask)
                    .unwrap_or_else(|error| panic!("reverse={reverse}, mask={mask}: {error:?}"));
                fixture.model.check(&promoted).unwrap();
            }
        }
    }

    #[test]
    fn exact_native_closure_oracle_detects_equal_payload_binding_substitution() {
        let fixture = IndependentNativeClosures::new(2, 1);
        let joined = fixture.join(false);
        fixture.model.check(&joined).unwrap();
        fixture.reject_equal_payload_substitution(&joined);
    }

    #[test]
    fn private_original_choices_refuse_ambiguous_issuer_groups() {
        let fixture = IndependentNativeClosures::new(2, 2);
        fixture.refuse_conflicting_choice(&fixture.join(false), false);
    }

    #[test]
    fn mixed_native_issuers_preserve_exact_closures_through_later_lexical_join() {
        let fixture = IndependentNativeClosures::new(3, 2);
        for reverse in [false, true] {
            let source = fixture.promote(&fixture.join(reverse), 3).unwrap();
            let namespace = source.namespace();
            let joined = fixture.join_later(&source, usize::from(reverse));
            fixture.model.check(&joined).unwrap();
            for (id, binding) in namespace {
                assert_eq!(joined.namespace()[&id], binding);
            }
            fixture
                .model
                .check(&fixture.promote(&joined, 3).unwrap())
                .unwrap();
        }
    }

    #[test]
    fn empty_projected_admission_validates_choices_and_narrows_namespace() {
        let fixture = IndependentNativeClosures::new(2, 2);
        let source = &fixture.sources[0];
        let inventory = source.inventory();
        let projection = CompilerInputProjection::from_issued_entries(&source.entries()).unwrap();
        let graph = source.capture_graph_selection();
        let nodes = inventory.node_count();
        for _ in 0..2 {
            let admitted = inventory
                .admit_projected_with_native_requirements(
                    source,
                    vec![],
                    &projection,
                    &[fixture.requirements[0].clone()],
                )
                .unwrap();
            assert_eq!(admitted.capture_graph_selection(), graph);
            assert_eq!(
                admitted.binding_dependencies(),
                source.binding_dependencies()
            );
            assert_eq!(inventory.node_count(), nodes);
        }
        let validation = crate::recovery_artifacts::PackageInterfaceValidation::default();
        let certified =
            crate::certified_products::CertifiedSourceSelection::from_compiler_projection(
                &projection,
                &source.metadata_snapshot(),
                &validation.inventory,
            )
            .unwrap();
        let certified_empty = inventory
            .admit_certified_with_demand(
                source,
                vec![],
                NativeArtifactDemand::ScopeInterfaces,
                &certified,
            )
            .unwrap();
        assert_eq!(certified_empty.capture_graph_selection(), graph);
        let narrowed = inventory
            .admit_projected_with_native_requirements(
                source,
                vec![],
                &projection.interface_only(),
                &[],
            )
            .unwrap();
        assert!(!narrowed
            .namespace()
            .contains_key(&fixture.originals[0].descriptor.id));
        assert_eq!(
            narrowed.capture_graph_selection().native_groups,
            graph.native_groups
        );
        assert_eq!(
            narrowed.binding_dependencies(),
            source.binding_dependencies()
        );
        assert_eq!(inventory.node_count(), nodes);
        fixture.refuse_conflicting_choice(&fixture.join(false), true);
    }

    #[test]
    fn empty_projected_admission_refuses_foreign_same_payload_native_requirement_atomically() {
        let fixture = IndependentNativeClosures::new(1, 1);
        let source = &fixture.sources[0];
        let inventory = source.inventory();
        let foreign = ArtifactInventory::default();
        let mut entries = source.entries();
        entries.push(Arc::new(entry("ForeignOnly", &[])));
        let foreign_view = foreign
            .admit_shared(&foreign.empty_view(), entries)
            .unwrap();
        let requirement = NativeOriginalRequirement::from_selected_view(
            fixture.originals[0].descriptor.id,
            &foreign_view,
        )
        .unwrap()
        .unwrap();
        assert_ne!(requirement, fixture.requirements[0]);
        let graph = source.capture_graph_selection();
        let rows = source.binding_dependencies();
        let nodes = inventory.node_count();
        let installed = inventory.metrics().installed_binding_rows;
        let projection = CompilerInputProjection::from_issued_entries(&source.entries()).unwrap();
        assert!(inventory
            .admit_projected_with_native_requirements(source, vec![], &projection, &[requirement])
            .is_err());
        assert_eq!(source.capture_graph_selection(), graph);
        assert_eq!(source.binding_dependencies(), rows);
        assert_eq!(inventory.node_count(), nodes);
        assert_eq!(inventory.metrics().installed_binding_rows, installed);
    }

    proptest::proptest! {
        #![proptest_config(proptest::test_runner::Config { cases: 32, ..Default::default() })]
        #[test]
        fn exact_group_custody_histories_preserve_selection_recovery_and_release(
            depth in 1usize..4, groups in 1u32..4,
            history in proptest::collection::vec(0u8..5, 0..16),
        ) {
            use fvm_ipld_blockstore::{Blockstore, MemoryBlockstore};
            use crate::exact_context::{Materializer, hydrate_selection};
            let fixture = IndependentNativeClosures::new(depth, groups);
            let joined = fixture.join(false);
            let mut view = joined.retain_original_custody(&joined.inventory().empty_view()).unwrap();
            let mut selected_roots = BTreeSet::new();
            let mut operations = vec![0, 1, 2, 3, 4];
            operations.extend(history);
            let mut inventories = vec![view.inventory().clone()];
            for operation in operations {
                view = match operation {
                    0 => {
                        selected_roots.clear();
                        view.retain_original_custody(&view.inventory().empty_view()).unwrap()
                    },
                    1 | 2 => {
                        let source = &fixture.sources[usize::from(operation - 1)];
                        selected_roots.extend(source.roots().iter().copied());
                        view.merge_selected(source, &fixture.interfaces).unwrap()
                    },
                    3 => {
                        let store = MemoryBlockstore::new();
                        let mut materializer = Materializer::new(&store);
                        let encoded = materializer.materialize_selection(None, view.selection_snapshot()).unwrap();
                        for (cid, bytes) in materializer.finish().unwrap().into_blocks() {
                            store.put_keyed(&cid, &bytes).unwrap();
                        }
                        let hydrated = hydrate_selection(&store, encoded.root()).unwrap();
                        let graph = hydrated.snapshot().graph_selection().unwrap();
                        proptest::prop_assert_eq!(&graph, &view.capture_graph_selection());
                        let inventory = ArtifactInventory::default();
                        let mut restored = inventory.recover_selection(&inventory.empty_view(), view.entries(), &graph, &view.binding_dependencies()).unwrap();
                        restored.bind_selection_snapshot_baseline(hydrated.snapshot().clone()).unwrap();
                        inventories.push(inventory);
                        restored
                    },
                    _ => {
                        let graph = view.capture_graph_selection();
                        let inventory = ArtifactInventory::default();
                        let mut forged = graph.clone();
                        let group = graph.native_groups[0];
                        forged.custody_roots.push(ArtifactBindingNode::Group(NativeGroupBinding {
                            binding: ArtifactBinding { selection: ArtifactSelectionId([0xff;32]), ..group.binding },
                            ..group
                        }));
                        proptest::prop_assert!(inventory.recover_selection(&inventory.empty_view(), view.entries(), &forged, &view.binding_dependencies()).is_err());
                        proptest::prop_assert_eq!(inventory.node_count(), 0);
                        view
                    },
                };
                fixture.model.check(&view).map_err(proptest::test_runner::TestCaseError::fail)?;
                let mut expected = selected_roots.clone();
                loop {
                    let before = expected.len();
                    let children = fixture.model.rows.iter().filter(|(from, _, _)| expected.contains(from)).map(|(_, to, _)| *to).collect::<Vec<_>>();
                    expected.extend(children);
                    if expected.len() == before { break; }
                }
                let expected_groups = expected.iter().filter_map(|node| match node {
                    ArtifactBindingNode::Group(group) => Some(NativeGroupKey { artifact: group.binding.artifact, original_ordinal: group.original_ordinal }),
                    _ => None,
                }).collect::<BTreeSet<_>>();
                proptest::prop_assert_eq!(view.roots().iter().copied().collect::<BTreeSet<_>>(), selected_roots.clone());
                proptest::prop_assert_eq!(view.selected_native_groups(), expected_groups.clone());
                proptest::prop_assert_eq!(view.metadata_snapshot().selected_native_groups, expected_groups);
                if selected_roots.is_empty() {
                    proptest::prop_assert!(view.namespace().is_empty());
                    for original in &fixture.originals {
                        proptest::prop_assert!(NativeOriginalRequirement::from_selected_view(original.descriptor.id, &view).unwrap().is_none());
                    }
                }
            }
            drop(view); drop(joined); drop(fixture);
            for inventory in inventories { proptest::prop_assert_eq!(inventory.node_count(), 0); }
        }
    }

    #[derive(Clone, Copy, Debug)]
    enum ExactClosureOp {
        Merge(u8),
        Narrow,
        Promote(u8),
        Join,
        Recover,
        RefuseIncompleteRecovery,
        RefuseConflictingChoice,
    }

    #[derive(Debug, Default)]
    struct ExactClosureCoverage {
        merge_orders: [usize; 2],
        repeated_sources: usize,
        narrowings: usize,
        private_subsets: [usize; 3],
        later_joins: usize,
        wrong_binding_mutants: usize,
        recoveries: usize,
        incomplete_refusals: usize,
        conflicting_choice_refusals: usize,
    }

    impl Drop for ExactClosureCoverage {
        fn drop(&mut self) {
            eprintln!("independent_native_closures coverage={self:?}");
        }
    }

    proptest::proptest! {
        #![proptest_config(proptest::test_runner::Config { cases: 32, ..Default::default() })]

        #[test]
        fn independent_native_closure_histories_preserve_exact_issuer_edges(
            depth in 1usize..4,
            groups in 1u32..4,
            generated in proptest::collection::vec((0u8..7, 1u8..4), 0..12),
        ) {
            let fixture = IndependentNativeClosures::new(depth, groups);
            let mut coverage = ExactClosureCoverage::default();
            let mut operations = generated.into_iter().map(|(operation, operand)| match operation {
                0 => ExactClosureOp::Merge(operand % 2),
                1 => ExactClosureOp::Narrow,
                2 => ExactClosureOp::Promote(operand),
                3 => ExactClosureOp::Recover,
                4 => ExactClosureOp::RefuseIncompleteRecovery,
                5 => ExactClosureOp::RefuseConflictingChoice,
                _ => ExactClosureOp::Join,
            }).collect::<Vec<_>>();
            let middle = operations.len() / 2;
            operations.splice(middle..middle, [ExactClosureOp::Promote(1), ExactClosureOp::Narrow,
                ExactClosureOp::Promote(2), ExactClosureOp::Recover, ExactClosureOp::Merge(0),
                ExactClosureOp::Merge(1), ExactClosureOp::Promote(3), ExactClosureOp::Join,
                ExactClosureOp::Promote(3), ExactClosureOp::Recover, ExactClosureOp::RefuseIncompleteRecovery,
                ExactClosureOp::RefuseConflictingChoice]);
            for reverse in [false, true] {
                coverage.merge_orders[usize::from(reverse)] += 1;
                let mut view = fixture.join(reverse);
                fixture.reject_equal_payload_substitution(&view);
                coverage.wrong_binding_mutants += 1;
                for operation in &operations {
                    view = match *operation {
                        ExactClosureOp::Merge(index) => {
                            coverage.repeated_sources += 1;
                            view.merge_selected(&fixture.sources[usize::from(index)], &fixture.interfaces).unwrap()
                        },
                        ExactClosureOp::Narrow => {
                            coverage.narrowings += 1;
                            view.with_compiler_projection(&fixture.interfaces).unwrap()
                        },
                        ExactClosureOp::Promote(mask) => {
                            coverage.private_subsets[usize::from(mask - 1)] += 1;
                            fixture.promote(&view, mask).map_err(|error| proptest::test_runner::TestCaseError::fail(
                                format!("reverse={reverse}, history={operations:?}, operation={operation:?}: {error:?}")))?
                        },
                        ExactClosureOp::Join => {
                            coverage.later_joins += 1;
                            fixture.join_later(&view, coverage.later_joins)
                        },
                        ExactClosureOp::Recover => {
                            let encoded = serde_json::to_vec(&view.capture_graph_selection()).unwrap();
                            let decoded = serde_json::from_slice(&encoded).unwrap();
                            let edges = serde_json::from_slice::<Vec<_>>(&serde_json::to_vec(&view.binding_dependencies()).unwrap()).unwrap();
                            let recovered = ArtifactInventory::default();
                            coverage.recoveries += 1;
                            recovered.recover_selection(&recovered.empty_view(), view.entries(), &decoded, &edges).unwrap()
                        },
                        ExactClosureOp::RefuseIncompleteRecovery => {
                            let refused = ArtifactInventory::default();
                            let mut incomplete = view.binding_dependencies();
                            incomplete.remove(0);
                            proptest::prop_assert!(refused.recover_selection(&refused.empty_view(), view.entries(),
                                &view.capture_graph_selection(), &incomplete).is_err());
                            proptest::prop_assert_eq!(refused.node_count(), 0);
                            coverage.incomplete_refusals += 1;
                            view
                        },
                        ExactClosureOp::RefuseConflictingChoice => {
                            fixture.refuse_conflicting_choice(&view, false);
                            coverage.conflicting_choice_refusals += 1;
                            view
                        },
                    };
                    fixture.model.check(&view).map_err(|error| proptest::test_runner::TestCaseError::fail(
                        format!("reverse={reverse}, history={operations:?}, operation={operation:?}: {error}")))?;
                }
            }
            proptest::prop_assert!(coverage.merge_orders.iter().all(|count| *count == 1));
            proptest::prop_assert!(coverage.private_subsets.iter().all(|count| *count >= 2));
            proptest::prop_assert!(coverage.recoveries >= 4 && coverage.repeated_sources >= 4);
            proptest::prop_assert!(coverage.later_joins >= 2);
            proptest::prop_assert_eq!(coverage.wrong_binding_mutants, 2);
            proptest::prop_assert!(coverage.incomplete_refusals >= 2 && coverage.conflicting_choice_refusals >= 2);
        }
    }

    #[test]
    fn inherited_and_foreign_bindings_reclaim_without_orphans() {
        for foreign in [false, true] {
            let inventory = ArtifactInventory::default();
            let empty = inventory.empty_view();
            let first = inventory.admit(&empty, vec![entry("A", &[])]).unwrap();
            let first_binding = first.capture_graph_selection().namespace;
            let joined = if foreign {
                let other = ArtifactInventory::default();
                let source = other
                    .admit(&other.empty_view(), vec![entry("B", &[])])
                    .unwrap();
                first.merge(&source).unwrap()
            } else {
                inventory.admit(&first, vec![entry("B", &[])]).unwrap()
            };
            assert_eq!(
                joined.artifact_ids().into_iter().collect::<BTreeSet<_>>(),
                BTreeSet::from([entry("A", &[]).descriptor.id, entry("B", &[]).descriptor.id])
            );
            assert!(joined
                .capture_graph_selection()
                .namespace
                .contains(&first_binding[0]));
            drop(joined);
            drop(first);
            drop(empty);
            assert_eq!(inventory.node_count(), 0, "foreign={foreign}");
        }
    }

    #[test]
    fn historical_source_import_does_not_select_current_publication_by_module_name() {
        use crate::declaration_context::ExactDeclarationContext;
        let old = Arc::new(issued_native_groups(
            "SourceEpoch",
            vec![(7, vec![])],
            &[],
            &BTreeMap::new(),
        ));
        let raw = crate::certified_products::tests::original_groups_fixture(
            "SourceEpoch",
            vec![(7, vec![])],
            2,
            &BTreeMap::new(),
        );
        let raw = crate::certified_products::fixture_finalized_product_with_requirements(
            raw,
            [2; 32],
            Some(BTreeMap::new()),
        );
        let new = Arc::new(
            ArtifactEntry::original(
                [2; 32],
                crate::certified_products::tests::recovered_witness_fixtures(&[raw])
                    .remove(0)
                    .product,
            )
            .unwrap(),
        );
        assert_eq!(old.descriptor.owner, new.descriptor.owner);
        assert_ne!(old.descriptor.id, new.descriptor.id);
        let (ArtifactPayload::Original(old_product), ArtifactPayload::Original(new_product)) =
            (&old.payload, &new.payload)
        else {
            unreachable!()
        };
        assert_ne!(old_product.owner(), new_product.owner());
        let inventory = ArtifactInventory::default();
        let old_view = inventory
            .admit(&inventory.empty_view(), vec![(*old).clone()])
            .unwrap();
        let target = Arc::new(ArtifactEntry::canonical(
            crate::certified_products::fixture_module_interface(
                [2; 32],
                "fixture",
                "SourceEpochTarget",
                BTreeMap::new(),
            ),
        ));
        let issuer_inventory = inventory.clone();
        let issuer = issuer_inventory
            .admit(
                &issuer_inventory.empty_view(),
                vec![(*new).clone(), (*target).clone()],
            )
            .unwrap();
        let projection =
            CompilerInputProjection::from_issued_entries(&[new.clone(), target.clone()]).unwrap();
        let context = ExactDeclarationContext::from_authenticated_execution(
            [2; 32],
            &issuer,
            projection.clone(),
            vec![
                crate::declaration_join::ExactLexicalNode {
                    owner: target.descriptor.owner.clone(),
                    imports: vec![new.descriptor.owner.clone()],
                },
                crate::declaration_join::ExactLexicalNode {
                    owner: new.descriptor.owner.clone(),
                    imports: vec![],
                },
            ],
            target.descriptor.owner.clone(),
            &[
                target.descriptor.owner.clone(),
                new.descriptor.owner.clone(),
            ],
        )
        .unwrap();
        let publication = context
            .issue_published_source_original("revision", "input", &new.descriptor.owner)
            .unwrap();
        let published = publication.context();
        let available = old_view
            .merge_selected(
                published.artifact_view(),
                published.compiler_input_projection(),
            )
            .unwrap();
        let current = available
            .retain_original_custody(published.artifact_view())
            .unwrap();
        let current = ExactDeclarationContext::from_authenticated_execution(
            [2; 32],
            &current,
            publication.context().compiler_input_projection().clone(),
            context.lexical_graph().to_vec(),
            new.descriptor.owner.clone(),
            &[new.descriptor.owner.clone()],
        )
        .unwrap();
        let historical_imports = [issued_source(&old, 7)];
        let historical_groups = current
            .artifact_view()
            .native_groups_for_target(None, &historical_imports)
            .unwrap();
        assert!(!historical_groups.is_empty());
        assert!(historical_groups
            .iter()
            .any(|group| group.binding.artifact == old.descriptor.id));
        assert!(current
            .published_source_original_selections_for_target(
                &historical_groups,
                &historical_imports
            )
            .unwrap()
            .is_empty());
        let selected_imports = [issued_source(&new, 7)];
        let selected_groups = current
            .artifact_view()
            .native_groups_for_target(None, &selected_imports)
            .unwrap();
        assert_eq!(
            current
                .published_source_original_selections_for_target(
                    &selected_groups,
                    &selected_imports
                )
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn certified_namespace_retains_and_reopens_historical_native_custody() {
        let inventory = ArtifactInventory::default();
        let old_native =
            issued_native_groups("Epoch", vec![(7, Vec::new())], &[], &BTreeMap::new());
        let native_id = old_native.descriptor.id;
        let ArtifactPayload::Original(product) = &old_native.payload else {
            unreachable!()
        };
        let old_carrier = ArtifactEntry::canonical(product.module_interface().unwrap().clone());
        let new_carrier = ArtifactEntry::canonical(crate::certified_products::fixture_module_core(
            product.module_interface().unwrap(),
            b"fresh Epoch Core".to_vec(),
        ));
        let old_id = old_carrier.descriptor.id;
        let new_id = new_carrier.descriptor.id;
        assert_ne!(old_id, new_id);
        let old = inventory
            .admit(&inventory.empty_view(), vec![old_native])
            .unwrap();
        let issuer_inventory = ArtifactInventory::default();
        let issuer = issuer_inventory
            .admit(&issuer_inventory.empty_view(), vec![new_carrier.clone()])
            .unwrap();
        let projection =
            CompilerInputProjection::from_issued_entries(&[Arc::new(new_carrier.clone())]).unwrap();
        let validation = crate::recovery_artifacts::PackageInterfaceValidation::default();
        let issued = crate::certified_products::CertifiedSourceSelection::from_compiler_projection(
            &projection,
            &issuer.metadata_snapshot(),
            &validation.inventory,
        )
        .unwrap();
        let current = inventory
            .admit_certified_with_demand(
                &old,
                vec![Arc::new(new_carrier)],
                NativeArtifactDemand::ScopeInterfaces,
                &issued,
            )
            .unwrap();
        assert_eq!(
            current
                .capture_graph_selection()
                .namespace
                .iter()
                .map(|binding| binding.artifact)
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([new_id])
        );
        assert!(current.artifact_ids().contains(&old_id));
        assert!(current.artifact_ids().contains(&native_id));
        assert!(
            current.merge(&old).is_err(),
            "ordinary incompatible namespace must still refuse"
        );
        let old_body = current.select_roots(vec![native_id]).unwrap();
        assert!(old_body.artifact_ids().contains(&old_id));
        assert!(!old_body.artifact_ids().contains(&new_id));
        old_body
            .native_requirements_from_roots(&[NativeRequirementRoot::Group {
                artifact: native_id,
                original_ordinal: 7,
            }])
            .unwrap();
        let graph = current.capture_graph_selection();
        let edges = current.binding_dependencies();
        let restored_inventory = ArtifactInventory::default();
        let restored = restored_inventory
            .recover_selection(
                &restored_inventory.empty_view(),
                current.entries(),
                &graph,
                &edges,
            )
            .unwrap();
        assert_eq!(restored.capture_graph_selection(), graph);
        let reopened = restored.select_roots(vec![native_id]).unwrap();
        assert!(reopened.artifact_ids().contains(&old_id));
        let mut missing = current.entries();
        missing.retain(|entry| entry.descriptor.id != old_id);
        let missing_inventory = ArtifactInventory::default();
        assert!(missing_inventory
            .recover_selection(&missing_inventory.empty_view(), missing, &graph, &edges)
            .is_err());
        assert_eq!(missing_inventory.node_count(), 0);
        drop(old);
        drop(current);
        drop(old_body);
        assert_eq!(inventory.node_count(), 0);
        drop(restored);
        drop(reopened);
        assert_eq!(restored_inventory.node_count(), 0);
    }

    #[test]
    fn later_native_group_keeps_inherited_carrier_binding() {
        let inventory = ArtifactInventory::default();
        let fixture = issued_native_groups(
            "Later",
            vec![(3, Vec::new()), (7, Vec::new())],
            &[],
            &BTreeMap::new(),
        );
        let ArtifactPayload::Original(product) = &fixture.payload else {
            unreachable!()
        };
        let product = crate::certified_products::tests::recovered_witness_fixtures(
            std::slice::from_ref(product),
        )
        .remove(0)
        .product;
        let native = ArtifactEntry::original([2; 32], product).unwrap();
        let native_id = native.descriptor.id;
        let carrier = inventory
            .admit_shared_with_demand(
                &inventory.empty_view(),
                vec![Arc::new(native.clone())],
                NativeArtifactDemand::ScopeInterfaces,
            )
            .unwrap();
        let initial = inventory
            .admit_shared_with_demand(
                &carrier,
                vec![Arc::new(native.clone())],
                NativeArtifactDemand::CertifiedTargetImports(&[issued_source(&native, 3)]),
            )
            .unwrap();
        let original_binding = initial
            .capture_graph_selection()
            .namespace
            .into_iter()
            .find(|binding| binding.artifact == native_id)
            .unwrap();
        assert_eq!(initial.selected_native_groups().len(), 1);
        let demanded = inventory
            .admit_shared_with_demand(
                &initial,
                vec![Arc::new(native.clone())],
                NativeArtifactDemand::CertifiedTargetImports(&[issued_source(&native, 7)]),
            )
            .unwrap();
        let graph = demanded.capture_graph_selection();
        let group = *graph
            .native_groups
            .iter()
            .find(|group| group.original_ordinal == 7)
            .unwrap();
        assert_ne!(group.binding.selection, original_binding.selection);
        assert!(!graph.bindings.contains(&group.binding));
        let edges = demanded.binding_dependencies();
        assert!(edges.contains(&(
            ArtifactBindingNode::Group(group),
            ArtifactBindingNode::Artifact(original_binding),
            ArtifactDependency::Interface,
        )));
        let recovery = ArtifactInventory::default();
        let restored = recovery
            .recover_selection(&recovery.empty_view(), demanded.entries(), &graph, &edges)
            .unwrap();
        assert_eq!(restored.capture_graph_selection(), graph);
        assert_eq!(restored.binding_dependencies(), edges);
        for view in [&demanded, &restored] {
            let ArtifactPayload::Original(product) = &native.payload else {
                unreachable!()
            };
            let canonical_id =
                ArtifactEntry::canonical(product.module_interface().unwrap().clone())
                    .descriptor
                    .id;
            let proof =
                NativeOriginalRequirement::from_issued_group_custody(native_id, canonical_id, view)
                    .unwrap()
                    .unwrap();
            assert!(proof
                .selection
                .graph_selection()
                .native_groups
                .iter()
                .any(|group| group.original_ordinal == 7
                    && group.binding.selection != original_binding.selection));
            assert!(proof
                .selection
                .graph_selection()
                .roots
                .iter()
                .all(|root| matches!(root, ArtifactBindingNode::Artifact(_))));
            view.native_requirements_from_roots(&[NativeRequirementRoot::Group {
                artifact: native_id,
                original_ordinal: 7,
            }])
            .unwrap();
            assert_eq!(
                view.dependencies(),
                initial.dependencies(),
                "internal group carrier edge must not become a public self dependency"
            );
        }
        let repeated = inventory
            .admit_shared_with_demand(
                &demanded,
                vec![Arc::new(entry("Unrelated", &[]))],
                NativeArtifactDemand::ScopeInterfaces,
            )
            .unwrap();
        assert_eq!(
            repeated.capture_graph_selection().native_groups,
            graph.native_groups
        );
        let reopened = repeated.select_roots(vec![native_id]).unwrap();
        reopened
            .native_requirements_from_roots(&[NativeRequirementRoot::Group {
                artifact: native_id,
                original_ordinal: 7,
            }])
            .unwrap();
        drop(carrier);
        drop(initial);
        drop(demanded);
        drop(repeated);
        drop(reopened);
        assert_eq!(inventory.node_count(), 0);
        drop(restored);
        assert_eq!(recovery.node_count(), 0);
    }

    #[test]
    fn compiler_namespace_extension_separates_selected_native_roots_from_custody() {
        let native = Arc::new(issued_native_groups(
            "Root",
            vec![(7, vec![])],
            &[],
            &BTreeMap::new(),
        ));
        let inventory = ArtifactInventory::default();
        let sources = ["Left", "Right"].map(|name| {
            inventory
                .admit_shared(
                    &inventory.empty_view(),
                    vec![native.clone(), Arc::new(entry(name, &[]))],
                )
                .unwrap()
        });
        let projections = sources
            .each_ref()
            .map(|source| CompilerInputProjection::from_issued_entries(&source.entries()).unwrap());
        let expected = sources[0].capture_graph_selection().native_groups;
        let historical = sources[1].capture_graph_selection().native_groups;
        assert_ne!(expected, historical);
        let unselected = sources[0].merge(&sources[1]).unwrap();
        assert!(
            matches!(unselected.merge_compiler_inputs(&projections[0], &inventory.empty_view(), &CompilerInputProjection::default()),
            Err(CompileError::ArtifactInventory(error))
                if matches!(error.failure, ArtifactInventoryFailure::BindingAmbiguity { .. })),
            "a raw union must retain conflicting explicit native roots"
        );
        let (selected, projection) = sources[0]
            .merge_compiler_inputs(&projections[0], &sources[1], &projections[1])
            .unwrap();
        let recovery = ArtifactInventory::default();
        let restored = recovery
            .recover_selection(
                &recovery.empty_view(),
                selected.entries(),
                &selected.capture_graph_selection(),
                &selected.binding_dependencies(),
            )
            .unwrap();
        for view in [&selected, &restored] {
            let requirements =
                NativeOriginalRequirement::for_projection(&projection, view, &[]).unwrap();
            let observed = requirements
                .iter()
                .flat_map(|requirement| requirement.selection.graph_selection().roots.iter())
                .filter_map(|node| match node {
                    ArtifactBindingNode::Group(group)
                        if group.binding.artifact == native.descriptor.id =>
                    {
                        Some(*group)
                    }
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert_eq!(observed, expected);
            let graph = view.capture_graph_selection();
            for group in expected.iter().chain(&historical) {
                assert!(
                    graph.native_groups.contains(group),
                    "independent historical closure stays in custody"
                );
            }
            assert_eq!(
                graph
                    .namespace
                    .iter()
                    .find(|binding| binding.artifact == native.descriptor.id),
                sources[0]
                    .capture_graph_selection()
                    .namespace
                    .iter()
                    .find(|binding| binding.artifact == native.descriptor.id)
            );
        }
    }

    #[test]
    fn compiler_support_extension_keeps_native_issuer_roots_and_historical_custody() {
        use crate::declaration_context::ExactDeclarationContext;
        let inventory = ArtifactInventory::default();
        let child = Arc::new(entry("Child", &[]));
        let native = Arc::new(issued_native_groups(
            "Root",
            vec![(7, Vec::new())],
            &[&child],
            &BTreeMap::new(),
        ));
        let old = inventory
            .admit_shared(
                &inventory.empty_view(),
                vec![native.clone(), child.clone(), Arc::new(entry("Left", &[]))],
            )
            .unwrap();
        let new = inventory
            .admit_shared(
                &inventory.empty_view(),
                vec![native.clone(), child, Arc::new(entry("Right", &[]))],
            )
            .unwrap();
        let old_projection = CompilerInputProjection::from_issued_entries(&old.entries()).unwrap();
        let new_projection = CompilerInputProjection::from_issued_entries(&new.entries()).unwrap();
        let owner = native.descriptor.owner.clone();
        let support = new_projection
            .source_support(&new, &BTreeSet::from([owner.clone()]))
            .unwrap();
        let context = ExactDeclarationContext::from_authenticated_execution(
            [2; 32],
            &old,
            old_projection,
            vec![],
            owner,
            &[],
        )
        .unwrap();
        let grown = context.extend_issued_compiler_support(&support).unwrap();
        let view = grown.artifact_view();
        let expected = old.capture_graph_selection().native_groups;
        let recovery = ArtifactInventory::default();
        let restored = recovery
            .recover_selection(
                &recovery.empty_view(),
                view.entries(),
                &view.capture_graph_selection(),
                &view.binding_dependencies(),
            )
            .unwrap();
        for (inventory, view) in [(&inventory, view), (&recovery, &restored)] {
            let requirements = NativeOriginalRequirement::for_projection(
                grown.compiler_input_projection(),
                view,
                &[],
            )
            .unwrap();
            let observed = requirements
                .iter()
                .flat_map(|requirement| requirement.selection.graph_selection().roots.iter())
                .filter_map(|root| match root {
                    InventoryNodeKey::Group(group)
                        if group.binding.artifact == native.descriptor.id =>
                    {
                        Some(*group)
                    }
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert_eq!(observed, expected);
            let mut certified =
                crate::certified_products::CertifiedSourceSelection::from_compiler_projection(
                    grown.compiler_input_projection(),
                    &view.metadata_snapshot(),
                    &crate::recovery_artifacts::PackageInterfaceValidation::default().inventory,
                )
                .unwrap();
            certified
                .with_native_original_requirements(&view.metadata_snapshot(), &requirements)
                .unwrap();
            let mut bound = inventory.bind_segment(view, vec![], &certified).unwrap();
            let selected = inventory
                .admit_bound_target(
                    &mut bound,
                    view,
                    NativeArtifactDemand::CertifiedTargetImports(&[issued_source(&native, 7)]),
                    vec![],
                    &certified,
                )
                .unwrap();
            let actual = selected
                .native_groups_for_target(None, &[issued_source(&native, 7)])
                .unwrap();
            assert_eq!(actual, expected.iter().copied().collect());
            assert!(new
                .capture_graph_selection()
                .native_groups
                .iter()
                .all(|group| selected
                    .capture_graph_selection()
                    .native_groups
                    .contains(group)));
        }
    }

    #[test]
    fn shared_native_group_keeps_each_issued_canonical_child_closure() {
        let inventory = ArtifactInventory::default();
        let child = entry("Child", &[]);
        let ArtifactPayload::Canonical(interface) = &child.payload else {
            unreachable!()
        };
        let next = ArtifactEntry::canonical(crate::certified_products::fixture_module_core(
            interface,
            b"new child Core".to_vec(),
        ));
        let native =
            issued_native_groups("Root", vec![(7, Vec::new())], &[&child], &BTreeMap::new());
        let native_id = native.descriptor.id;
        let old_child_id = child.descriptor.id;
        let new_child_id = next.descriptor.id;
        let old = inventory
            .admit(&inventory.empty_view(), vec![native.clone(), child])
            .unwrap();
        let original = old.select_roots(vec![native_id]).unwrap();
        let facts = ExactArtifactSelection::capture(&original);
        let new = inventory
            .admit(&inventory.empty_view(), vec![native, next])
            .unwrap();
        assert_eq!(old.selected_native_groups(), new.selected_native_groups());
        assert_ne!(
            old.capture_graph_selection().native_groups,
            new.capture_graph_selection().native_groups
        );
        let projection = CompilerInputProjection::from_issued_entries(&new.entries()).unwrap();
        let grown = original.merge_selected(&new, &projection).unwrap();
        let reopened = grown.select_issued(vec![native_id], &facts).unwrap();
        assert!(reopened.artifact_ids().contains(&old_child_id));
        assert!(!reopened.artifact_ids().contains(&new_child_id));
        reopened
            .native_requirements_from_roots(&[NativeRequirementRoot::Group {
                artifact: native_id,
                original_ordinal: 7,
            }])
            .unwrap();
        let recovery = ArtifactInventory::default();
        let restored = recovery
            .recover_selection(
                &recovery.empty_view(),
                grown.entries(),
                &grown.capture_graph_selection(),
                &grown.binding_dependencies(),
            )
            .unwrap();
        let restored_old = restored.select_issued(vec![native_id], &facts).unwrap();
        assert_eq!(
            restored_old.capture_graph_selection(),
            reopened.capture_graph_selection()
        );
        drop(old);
        drop(original);
        drop(new);
        drop(grown);
        drop(reopened);
        assert_eq!(inventory.node_count(), 0);
        drop(restored);
        drop(restored_old);
        assert_eq!(recovery.node_count(), 0);
    }

    #[test]
    fn issued_selection_reopens_original_namespace_from_grown_custody() {
        let inventory = ArtifactInventory::default();
        let root = entry("Root", &["Child"]);
        let old_child = entry("Child", &[]);
        let ArtifactPayload::Canonical(interface) = &old_child.payload else {
            unreachable!()
        };
        let new_child = ArtifactEntry::canonical(crate::certified_products::fixture_module_core(
            interface,
            b"new child Core".to_vec(),
        ));
        let old = inventory
            .admit(&inventory.empty_view(), vec![root.clone(), old_child])
            .unwrap();
        let original = old.select_roots(vec![root.descriptor.id]).unwrap();
        let facts = ExactArtifactSelection::capture(&original);
        let new = inventory
            .admit(
                &inventory.empty_view(),
                vec![root.clone(), new_child.clone()],
            )
            .unwrap();
        let projection = CompilerInputProjection::from_issued_entries(&[
            Arc::new(root.clone()),
            Arc::new(new_child),
        ])
        .unwrap();
        let grown = original.merge_selected(&new, &projection).unwrap();
        let reopened = grown
            .select_issued(vec![root.descriptor.id], &facts)
            .unwrap();
        assert_eq!(
            reopened.capture_graph_selection(),
            original.capture_graph_selection()
        );
        let ambiguous = original
            .merge_selected(&new, &CompilerInputProjection::default())
            .unwrap();
        assert!(
            matches!(ambiguous.select_roots(vec![root.descriptor.id]), Err(CompileError::ArtifactInventory(error)) if matches!(error.failure, ArtifactInventoryFailure::BindingAmbiguity { .. }))
        );
        assert!(
            original.merge(&new).is_err(),
            "two incompatible selected child versions must refuse"
        );
        drop(original);
        drop(old);
        drop(new);
        drop(grown);
        drop(reopened);
        drop(ambiguous);
        assert_eq!(inventory.node_count(), 0);
    }

    #[test]
    fn binding_recovery_refuses_changed_digest_edges_and_unused_witnesses() {
        let inventory = ArtifactInventory::default();
        let view = inventory
            .admit(
                &inventory.empty_view(),
                vec![entry("Root", &["Child"]), entry("Child", &[])],
            )
            .unwrap();
        let issued = ExactArtifactSelection::capture(&view);
        assert!(Arc::ptr_eq(
            &issued.graph_selection().selections[0],
            &view.capture_graph_selection().selections[0],
        ));
        let encoded = serde_json::to_vec(issued.graph_selection()).unwrap();
        let decoded = ExactArtifactSelection::from_recovered_graph_selection(
            serde_json::from_slice(&encoded).unwrap(),
        );
        assert_eq!(decoded, issued);
        let reopened = view.select_issued(view.artifact_ids(), &decoded).unwrap();
        assert_eq!(
            reopened.capture_graph_selection(),
            view.capture_graph_selection()
        );
        for mutation in 0..3 {
            let mut graph = view.capture_graph_selection();
            let mut edges = view.binding_dependencies();
            match mutation {
                0 => {
                    Arc::make_mut(&mut graph.selections[0]).selection =
                        ArtifactSelectionId([0xee; 32])
                }
                1 => edges.clear(),
                _ => {
                    let extra = ArtifactInventory::default();
                    let extra = extra
                        .admit(&extra.empty_view(), vec![entry("Unused", &[])])
                        .unwrap();
                    graph
                        .selections
                        .extend(extra.capture_graph_selection().selections);
                }
            }
            let transported = ExactArtifactSelection::from_recovered_graph_selection(
                serde_json::from_slice(&serde_json::to_vec(&graph).unwrap()).unwrap(),
            );
            let recovered = ArtifactInventory::default();
            assert!(recovered
                .recover_selection(
                    &recovered.empty_view(),
                    view.entries(),
                    transported.graph_selection(),
                    &edges,
                )
                .is_err());
            assert_eq!(recovered.node_count(), 0, "mutation={mutation}");
        }
    }

    #[test]
    fn hydrated_snapshot_baseline_preserves_exact_facts_and_native_lease_custody() {
        let inventory = ArtifactInventory::default();
        let (native, values, _) = native_scope_fixture();
        let unused = entry_in_unit("fixture", "Unused", vec![]);
        let unused_id = unused.descriptor.id;
        let carriers = inventory
            .admit_shared_with_demand(
                &inventory.empty_view(),
                vec![Arc::new(native.clone()), Arc::new(values), Arc::new(unused)],
                NativeArtifactDemand::ScopeInterfaces,
            )
            .unwrap();
        let selected = inventory
            .admit_shared_with_demand(
                &carriers,
                vec![Arc::new(native.clone())],
                NativeArtifactDemand::AllGroups,
            )
            .unwrap()
            .select_admission_roots(vec![native.descriptor.id])
            .unwrap();
        let baseline = Arc::clone(selected.selection_snapshot());
        let graph = selected.capture_graph_selection();
        assert!(!selected.artifact_ids().contains(&unused_id));
        assert!(graph
            .selections
            .iter()
            .any(|proof| proof.nodes.iter().any(|node| node.artifact() == unused_id)));
        let recovered_inventory = ArtifactInventory::default();
        let mut recovered = recovered_inventory
            .recover_selection(
                &recovered_inventory.empty_view(),
                selected.entries(),
                &graph,
                &selected.binding_dependencies(),
            )
            .unwrap();
        let issued = Arc::clone(recovered.selection_snapshot());
        let live_nodes = recovered_inventory.node_count();
        for mutation in 0..5 {
            let mut changed = baseline.as_ref().clone();
            match mutation {
                0 => changed.roots.clear(),
                1 => changed.namespace.clear(),
                2 => {
                    changed
                        .custody_roots
                        .insert(ArtifactBindingNode::Artifact(graph.bindings[0]), ());
                }
                3 => {
                    let (scope, reference) = changed.scopes.iter().next().unwrap();
                    let scope = *scope;
                    let mut proof = Arc::clone(reference.proof());
                    let duplicate = proof.nodes[0];
                    Arc::make_mut(&mut proof).nodes.push(duplicate);
                    changed
                        .scopes
                        .insert(scope, ImmutableScopeProofRef::from_issued(proof));
                }
                _ => {
                    changed.scopes.remove(&graph.selections[0].selection);
                }
            }
            assert!(recovered
                .bind_selection_snapshot_baseline(Arc::new(changed))
                .is_err());
            assert!(Arc::ptr_eq(recovered.selection_snapshot(), &issued));
            assert_eq!(recovered_inventory.node_count(), live_nodes);
            assert_eq!(recovered.capture_graph_selection(), graph);
        }
        recovered
            .bind_selection_snapshot_baseline(Arc::clone(&baseline))
            .unwrap();
        assert!(Arc::ptr_eq(recovered.selection_snapshot(), &baseline));
        assert_eq!(recovered.capture_graph_selection(), graph);
        assert_eq!(recovered_inventory.node_count(), live_nodes);
        let group = graph.native_groups[0];
        assert_eq!(
            scoped_group_child(&recovered, group),
            scoped_group_child(&selected, group)
        );
        drop(recovered);
        assert_eq!(recovered_inventory.node_count(), 0);
        assert!(!baseline.scopes.is_empty());
    }

    #[test]
    fn incremental_binding_scopes_keep_only_new_source_rows() {
        let inventory = ArtifactInventory::default();
        let mut view = inventory
            .admit(
                &inventory.empty_view(),
                (0..12)
                    .map(|index| entry(&format!("Base{index}"), &[]))
                    .collect(),
            )
            .unwrap();
        for generation in 0..4 {
            if generation != 0 {
                view = inventory
                    .admit(&view, vec![entry(&format!("Added{generation}"), &[])])
                    .unwrap();
            }
            let graph = view.capture_graph_selection();
            let observed = view.capture_graph_selection();
            for (issued, borrowed) in graph.selections.iter().zip(&observed.selections) {
                assert!(Arc::ptr_eq(issued, borrowed));
            }
            let mut plan_sizes = graph
                .selections
                .iter()
                .map(|plan| plan.nodes.len())
                .collect::<Vec<_>>();
            plan_sizes.sort();
            let mut expected = vec![1; generation];
            expected.push(12);
            assert_eq!(plan_sizes, expected);
            assert_eq!(graph.bindings.len(), 12 + generation);
            let unique_witness_bytes = graph
                .selections
                .iter()
                .map(|plan| serde_json::to_vec(plan.as_ref()).unwrap().len())
                .sum::<usize>();
            eprintln!(
                "binding-plan-history generation={generation} live_bindings={} plans={} unique_witness_nodes={} unique_witness_bytes={unique_witness_bytes}",
                graph.bindings.len(),
                graph.selections.len(),
                plan_sizes.iter().sum::<usize>()
            );
            let restored_inventory = ArtifactInventory::default();
            let restored = restored_inventory
                .recover_selection(
                    &restored_inventory.empty_view(),
                    view.entries(),
                    &graph,
                    &view.binding_dependencies(),
                )
                .unwrap();
            assert_eq!(restored.capture_graph_selection(), graph);
            for (issued, restored) in graph
                .selections
                .iter()
                .zip(&restored.capture_graph_selection().selections)
            {
                assert!(Arc::ptr_eq(issued, restored));
            }
            drop(restored);
            assert_eq!(restored_inventory.node_count(), 0);
        }
        drop(view);
        assert_eq!(inventory.node_count(), 0);
    }

    proptest::proptest! {
        #![proptest_config(proptest::test_runner::Config::with_cases(24))]
        #[test]
        fn binding_histories_preserve_issued_children_and_release_payloads(
            choices in proptest::collection::vec(0u8..4, 1..12),
            warm in proptest::bool::ANY,
            drop_issuer in proptest::bool::ANY,
        ) {
            let inventory = ArtifactInventory::default();
            let root = entry("Root", &["Child"]); let child = entry("Child", &[]);
            let ArtifactPayload::Canonical(base) = &child.payload else { unreachable!() };
            let mut views = Vec::new(); let mut expected = Vec::new();
            for choice in choices {
                let next = ArtifactEntry::canonical(crate::certified_products::fixture_module_core(base, format!("child Core {choice}").into_bytes()));
                let child_id = next.descriptor.id;
                let issued = inventory.admit(&inventory.empty_view(), vec![root.clone(), next]).unwrap();
                if warm { proptest::prop_assert!(issued.artifact_ids().contains(&child_id)); }
                let selected = issued.select_roots(vec![root.descriptor.id]).unwrap();
                let facts = ExactArtifactSelection::capture(&selected);
                if drop_issuer { drop(issued); } else { views.push(issued); expected.push(child_id); }
                let reopened = selected.select_issued(vec![root.descriptor.id], &facts).unwrap();
                proptest::prop_assert_eq!(reopened.capture_graph_selection().namespace, selected.capture_graph_selection().namespace);
                views.push(reopened); expected.push(child_id);
            }
            for (view, child_id) in views.iter().zip(&expected) {
                proptest::prop_assert_eq!(view.artifact_ids().into_iter().collect::<BTreeSet<_>>(), BTreeSet::from([root.descriptor.id, *child_id]));
                proptest::prop_assert_eq!(view.interface_dependencies(), vec![(root.descriptor.id, *child_id, ArtifactDependency::Interface)]);
                let graph = view.capture_graph_selection();
                let restored = ArtifactInventory::default();
                let recovered = restored.recover_selection(&restored.empty_view(), view.entries(), &graph, &view.binding_dependencies()).unwrap();
                proptest::prop_assert_eq!(recovered.capture_graph_selection(), graph);
                drop(recovered); proptest::prop_assert_eq!(restored.node_count(), 0);
            }
            drop(views); proptest::prop_assert_eq!(inventory.node_count(), 0);
        }
    }

    #[test]
    fn independent_native_admission_does_not_inherit_global_source_witness() {
        let cold = native_entry("Original", &[]);
        let mut fresh = cold.clone();
        let ArtifactPayload::Original(cold_product) = &cold.payload else {
            unreachable!()
        };
        fresh.payload = ArtifactPayload::Original(cold_product.clone().with_source_sha256([1; 32]));
        let ArtifactPayload::Original(fresh_product) = &fresh.payload else {
            unreachable!()
        };
        assert_eq!(cold, fresh);
        assert_ne!(cold_product, fresh_product);
        let mut observations = Vec::new();
        for (existing, incoming, expected) in
            [(&fresh, &cold, None), (&cold, &fresh, Some([1; 32]))]
        {
            let inventory = ArtifactInventory::default();
            let old = inventory
                .admit(&inventory.empty_view(), vec![existing.clone()])
                .unwrap();
            let unrelated = inventory
                .admit(&inventory.empty_view(), vec![incoming.clone()])
                .unwrap();
            let selected = unrelated
                .entries()
                .into_iter()
                .find(|entry| entry.descriptor.id == incoming.descriptor.id)
                .unwrap();
            let ArtifactPayload::Original(product) = &selected.payload else {
                unreachable!()
            };
            eprintln!(
                "independent admission: expected_source={expected:?}, actual_source={:?}, equals_fresh_product={}",
                product.source_sha256(),
                product == fresh_product
            );
            observations.push((expected, product.source_sha256()));
            let old_entries = old.entries();
            let ArtifactPayload::Original(old_product) = &old_entries
                .iter()
                .find(|entry| entry.descriptor.id == existing.descriptor.id)
                .unwrap()
                .payload
            else {
                unreachable!()
            };
            let ArtifactPayload::Original(existing_product) = &existing.payload else {
                unreachable!()
            };
            assert_eq!(old_product, existing_product);
            let joined = unrelated.merge(&old).unwrap();
            let selected = joined.select_roots(vec![incoming.descriptor.id]).unwrap();
            drop(joined);
            drop(unrelated);
            drop(old);
            let source_of = |view: &ArtifactView| {
                let entries = view.entries();
                let ArtifactPayload::Original(product) = &entries
                    .iter()
                    .find(|entry| entry.descriptor.id == incoming.descriptor.id)
                    .unwrap()
                    .payload
                else {
                    unreachable!()
                };
                product.source_sha256()
            };
            assert_eq!(source_of(&selected), expected);
            let fresh_again = inventory
                .admit(&inventory.empty_view(), vec![fresh.clone()])
                .unwrap();
            assert_eq!(source_of(&selected.merge(&fresh_again).unwrap()), expected);
        }
        assert!(
            observations
                .iter()
                .all(|(expected, actual)| expected == actual),
            "unrelated retained artifacts cannot replace a receiving source witness: {observations:?}"
        );
    }

    proptest::proptest! {
        #![proptest_config(proptest::test_runner::Config {
            cases: 64,
            max_shrink_iters: 4096,
            failure_persistence: option_env!("TIDEPOOL_PROPTEST_REGRESSIONS").map(|path| Box::new(proptest::test_runner::FileFailurePersistence::Direct(path)) as Box<dyn proptest::test_runner::FailurePersistence>),
            ..proptest::test_runner::Config::default()
        })]
        #[test]
        fn source_witness_histories_follow_selected_issuer(
            operations in proptest::collection::vec((0u8..5, 0u8..3, proptest::bool::ANY), 1..40)
        ) {
            let base = native_entry("Original", &[]);
            let id = base.descriptor.id;
            let issue = |choice| {
                let mut entry = base.clone();
                if choice != 0 {
                    let ArtifactPayload::Original(product) = &entry.payload else { unreachable!() };
                    entry.payload = ArtifactPayload::Original(product.clone().with_source_sha256([choice; 32]));
                }
                entry
            };
            let witness = |view: &ArtifactView| {
                let entries = view.entries();
                let entry = entries.iter().find(|entry| entry.descriptor.id == id).unwrap();
                let ArtifactPayload::Original(product) = &entry.payload else { unreachable!() };
                product.source_sha256()
            };
            let inventory = ArtifactInventory::default();
            let mut current = inventory.admit(&inventory.empty_view(), vec![issue(0)]).unwrap();
            let mut expected = None;
            let mut old_views = Vec::new();
            let mut operation_counts = [0usize; 5];
            let mut warm_reads = 0usize;
            let mut delayed_reads = 0usize;
            for (operation, choice, warm) in operations {
                operation_counts[operation as usize] += 1;
                if warm { proptest::prop_assert_eq!(witness(&current), expected); }
                old_views.push((current.clone(), expected));
                let incoming_expected = if choice == 0 { None } else { Some([choice; 32]) };
                match operation {
                    0 => {
                        current = inventory.admit(&inventory.empty_view(), vec![issue(choice)]).unwrap();
                        expected = incoming_expected;
                    }
                    1 => { current = inventory.admit(&current, vec![issue(choice)]).unwrap(); }
                    2 | 3 => {
                        let other_inventory = if operation == 2 { inventory.clone() } else { ArtifactInventory::default() };
                        let incoming = other_inventory.admit(&other_inventory.empty_view(), vec![issue(choice)]).unwrap();
                        proptest::prop_assert_eq!(witness(&incoming), incoming_expected);
                        current = current.merge(&incoming).unwrap();
                    }
                    _ => { current = current.select_roots(vec![id]).unwrap(); }
                }
                if warm {
                    warm_reads += 1;
                    proptest::prop_assert_eq!(witness(&current), expected);
                    let (old, proof) = &old_views[choice as usize % old_views.len()];
                    proptest::prop_assert_eq!(witness(old), *proof);
                    // Unchanged issuing authority does not copy a map per append.
                    let appended = inventory.admit_shared(&current, current.entries()).unwrap();
                    proptest::prop_assert_eq!(appended.retained_issuing_payload_handles(), current.retained_issuing_payload_handles());
                    current = appended;
                } else {
                    delayed_reads += 1;
                }
            }
            proptest::prop_assert_eq!(witness(&current), expected);
            for (old, proof) in &old_views { proptest::prop_assert_eq!(witness(old), *proof); }
            eprintln!("source_witness_history operations={operation_counts:?} warm_reads={warm_reads} delayed_reads={delayed_reads}");
        }
    }

    #[test]
    fn native_inventory_reuse_preserves_each_owners_source_admission_witness() {
        let cold = native_entry("Original", &[]);
        let mut fresh = cold.clone();
        let ArtifactPayload::Original(product) = &cold.payload else {
            unreachable!()
        };
        fresh.payload = ArtifactPayload::Original(product.clone().with_source_sha256([1; 32]));
        let ArtifactPayload::Original(fresh_product) = &fresh.payload else {
            unreachable!()
        };
        assert_ne!(product, fresh_product);
        assert_eq!(cold, fresh);

        let cold_inventory = ArtifactInventory::default();
        let cold_view = cold_inventory
            .admit(&cold_inventory.empty_view(), vec![cold.clone()])
            .unwrap();
        let fresh_inventory = ArtifactInventory::default();
        let fresh_view = fresh_inventory
            .admit(&fresh_inventory.empty_view(), vec![fresh.clone()])
            .unwrap();
        for (retained, incoming, expected_source) in [
            (&cold_view, &fresh_view, None),
            (&fresh_view, &cold_view, Some([1; 32])),
        ] {
            let merged = retained.merge(incoming).unwrap();
            let selected = merged
                .entries()
                .into_iter()
                .find(|entry| entry.descriptor.id == cold.descriptor.id)
                .unwrap();
            let ArtifactPayload::Original(product) = &selected.payload else {
                unreachable!()
            };
            assert_eq!(product.source_sha256(), expected_source);
        }

        let altered = CertifiedRecoveryProduct::from_certification(
            product.owner().clone(),
            product.interface_bytes().to_vec(),
            b"changed original payload".to_vec(),
            product.package_imports_bytes().to_vec(),
            product.certification_bytes().to_vec(),
        )
        .with_module_interface(product.module_interface().unwrap().clone())
        .unwrap();
        let mut conflicting_bytes = cold.clone();
        conflicting_bytes.payload = ArtifactPayload::Original(altered);
        assert!(matches!(
            cold_inventory.admit(&cold_view, vec![conflicting_bytes]),
            Err(CompileError::ArtifactInventory(error))
                if matches!(error.failure, ArtifactInventoryFailure::MetadataConflict { artifact }
                    if artifact == cold.descriptor.id)
        ));
        assert_eq!(cold_view.artifact_ids(), fresh_view.artifact_ids());
    }

    #[test]
    fn joined_interface_requirements_are_sets_without_discarding_changed_edges() {
        let canonical = crate::certified_products::fixture_module_interface(
            [2; 32],
            "unit",
            "Joined",
            BTreeMap::new(),
        );
        let interface = CertifiedJoinedInterface::from_certification(
            [2; 32],
            "unit".into(),
            "Joined".into(),
            canonical.interface_bytes().to_vec(),
            canonical.package_imports_bytes().to_vec(),
        )
        .unwrap();
        let joined = |requirements| {
            ArtifactEntry::interface(
                interface.clone(),
                JoinedInterfaceRole::LexicalJoin,
                requirements,
            )
        };
        let first = joined(vec![module("B"), module("A"), module("B")]);
        let second = joined(vec![module("A"), module("B")]);
        assert_eq!(first, second);
        let inventory = ArtifactInventory::default();
        let view = inventory
            .admit(
                &inventory.empty_view(),
                vec![entry("A", &[]), entry("B", &[]), first.clone()],
            )
            .unwrap();
        assert_eq!(
            inventory.admit(&view, vec![second]).unwrap().artifact_ids(),
            view.artifact_ids()
        );
        assert!(matches!(
            inventory.admit(&view, vec![joined(vec![module("A")])]),
            Err(CompileError::ArtifactInventory(error))
                if matches!(error.failure, ArtifactInventoryFailure::MetadataConflict { artifact }
                    if artifact == first.descriptor.id)
        ));
    }

    #[test]
    fn native_variants_keep_exact_carriers_and_require_explicit_materialization_selection() {
        let inventory = ArtifactInventory::default();
        let empty = inventory.empty_view();
        let first = issued_native_groups("Original", vec![(0, Vec::new())], &[], &BTreeMap::new());
        let second = ArtifactEntry::original(
            [2; 32],
            crate::certified_products::fixture_finalized_product(
                crate::certified_products::tests::original_groups_fixture(
                    "Original",
                    vec![(0, Vec::new())],
                    2,
                    &BTreeMap::new(),
                ),
                [2; 32],
            ),
        )
        .unwrap();
        let original_owner = first.descriptor.owner.clone();
        let consumer = issued_native_groups(
            "Consumer",
            vec![(0, vec![issued_source(&second, 0)])],
            &[&second],
            &BTreeMap::new(),
        );
        let view = inventory
            .admit(
                &empty,
                vec![first.clone(), second.clone(), consumer.clone()],
            )
            .unwrap();
        let metadata = view.metadata_snapshot();
        assert_eq!(
            metadata.ambiguous_native_owners,
            BTreeSet::from([original_owner.clone()])
        );
        assert!(view
            .entries_for_owners(std::iter::once(original_owner.clone()))
            .is_err());
        let carrier = match &first.payload {
            ArtifactPayload::Original(product) => {
                ArtifactEntry::canonical(product.module_interface().unwrap().clone())
                    .descriptor
                    .id
            }
            _ => unreachable!(),
        };
        let dependencies = view.dependencies();
        for native in [&first, &second] {
            assert!(dependencies.contains(&(
                native.descriptor.id,
                carrier,
                ArtifactDependency::Interface
            )));
        }
        assert!(dependencies.contains(&(
            consumer.descriptor.id,
            second.descriptor.id,
            ArtifactDependency::NativeGroup {
                dependent_ordinal: 0,
                required_ordinal: 0
            }
        )));
        assert!(!dependencies.contains(&(
            consumer.descriptor.id,
            first.descriptor.id,
            ArtifactDependency::NativeGroup {
                dependent_ordinal: 0,
                required_ordinal: 0
            }
        )));
        let selected = view.select_roots(vec![first.descriptor.id]).unwrap();
        assert!(selected
            .metadata_snapshot()
            .ambiguous_native_owners
            .is_empty());
        assert_eq!(
            selected
                .entries_for_owners(std::iter::once(original_owner.clone()))
                .unwrap()[&original_owner]
                .descriptor
                .id,
            first.descriptor.id
        );
        let types = view.interface_projection(&[original_owner]).unwrap();
        assert_eq!(types.artifact_ids(), vec![carrier]);
        drop(view);
        assert_eq!(inventory.node_count(), 3);
        drop(selected);
        assert_eq!(inventory.node_count(), 1);
        drop(types);
        assert_eq!(inventory.node_count(), 0);
    }

    #[test]
    fn projected_original_preserves_unselected_exact_native_dependency_custody() {
        let leaf = issued_native_groups("Leaf", vec![(7, Vec::new())], &[], &BTreeMap::new());
        let old = issued_native_groups(
            "Helper",
            vec![(7, vec![issued_source(&leaf, 7)])],
            &[&leaf],
            &BTreeMap::new(),
        );
        let root = issued_native_groups(
            "Root",
            vec![(3, Vec::new()), (29, vec![issued_source(&old, 7)])],
            &[&old],
            &BTreeMap::new(),
        );
        let ArtifactPayload::Original(root_product) = &root.payload else {
            unreachable!()
        };
        let root = ArtifactEntry::original(
            [2; 32],
            crate::certified_products::tests::recovered_witness_fixtures(&[root_product.clone()])
                .remove(0)
                .product,
        )
        .unwrap();
        let new = ArtifactEntry::original(
            [2; 32],
            crate::certified_products::fixture_finalized_product_with_requirements(
                crate::certified_products::tests::original_groups_fixture(
                    "Helper",
                    vec![(7, vec![issued_source(&leaf, 7)])],
                    2,
                    &BTreeMap::new(),
                ),
                [2; 32],
                Some(BTreeMap::from([(
                    (
                        leaf.descriptor.owner.unit.clone(),
                        leaf.descriptor.owner.module.clone(),
                    ),
                    leaf.descriptor.interface_sha256,
                )])),
            ),
        )
        .unwrap();
        assert_ne!(old.descriptor.id, new.descriptor.id);
        assert_eq!(
            old.descriptor.interface_sha256,
            new.descriptor.interface_sha256
        );
        let early = NativeGroupKey {
            artifact: root.descriptor.id,
            original_ordinal: 3,
        };
        let late = NativeGroupKey {
            artifact: root.descriptor.id,
            original_ordinal: 29,
        };
        let unavailable_inventory = ArtifactInventory::default();
        let ambient = unavailable_inventory
            .admit(
                &unavailable_inventory.empty_view(),
                vec![old.clone(), leaf.clone()],
            )
            .unwrap();
        let unavailable = unavailable_inventory
            .admit_recovery_selection(
                &unavailable_inventory.empty_view(),
                vec![root.clone(), new.clone(), leaf.clone()]
                    .into_iter()
                    .map(Arc::new)
                    .collect(),
                &BTreeSet::from([early]),
            )
            .unwrap()
            .select_roots(vec![root.descriptor.id])
            .unwrap();
        assert!(!unavailable.artifact_ids().contains(&old.descriptor.id));
        assert!(!unavailable.artifact_ids().contains(&new.descriptor.id));
        assert!(matches!(
            unavailable_inventory.admit_shared_with_demand(
                &unavailable,
                vec![Arc::new(root.clone())],
                NativeArtifactDemand::CertifiedTargetImports(&[issued_source(&root, 29)]),
            ),
            Err(CompileError::ArtifactInventory(error))
                if matches!(error.failure, ArtifactInventoryFailure::MissingDependency { .. })
        ));
        drop(unavailable);
        drop(ambient);
        assert_eq!(unavailable_inventory.node_count(), 0);
        for cross_inventory in [false, true] {
            let inventory = ArtifactInventory::default();
            let available = inventory
                .admit_recovery_selection(
                    &inventory.empty_view(),
                    vec![root.clone(), old.clone(), leaf.clone()]
                        .into_iter()
                        .map(Arc::new)
                        .collect(),
                    &BTreeSet::from([early]),
                )
                .unwrap();
            let selected = available.select_roots(vec![root.descriptor.id]).unwrap();
            assert_eq!(selected.selected_native_groups(), BTreeSet::from([early]));
            assert_eq!(
                selected
                    .root_entries()
                    .iter()
                    .map(|entry| entry.descriptor.id)
                    .collect::<Vec<_>>(),
                [root.descriptor.id]
            );
            assert!(selected.artifact_ids().contains(&old.descriptor.id));
            assert!(selected.artifact_ids().contains(&leaf.descriptor.id));
            let types = selected
                .interface_projection(&[root.descriptor.owner.clone()])
                .unwrap();
            assert!(types.entries().iter().all(|entry| !entry.is_native()));
            let projection =
                CompilerInputProjection::from_issued_entries(&[Arc::new(root.clone())]).unwrap();
            assert_eq!(
                projection.roles().len(),
                1,
                "custody grants no compiler namespace role"
            );
            let destination = ArtifactInventory::default();
            let retained = if cross_inventory {
                let existing = destination
                    .admit(&destination.empty_view(), vec![entry("Unrelated", &[])])
                    .unwrap();
                existing.merge(&selected).unwrap()
            } else {
                selected.clone()
            };
            drop(types);
            drop(selected);
            drop(available);
            if cross_inventory {
                assert_eq!(inventory.node_count(), 0);
            }
            let branch = retained
                .inventory()
                .admit_shared_with_demand(
                    &retained,
                    vec![Arc::new(new.clone())],
                    NativeArtifactDemand::ScopeInterfaces,
                )
                .unwrap();
            let demanded = branch
                .inventory()
                .admit_shared_with_demand(
                    &branch,
                    vec![Arc::new(root.clone())],
                    NativeArtifactDemand::CertifiedTargetImports(&[issued_source(&root, 29)]),
                )
                .unwrap();
            assert_eq!(
                demanded.selected_native_groups(),
                BTreeSet::from([
                    early,
                    late,
                    NativeGroupKey {
                        artifact: old.descriptor.id,
                        original_ordinal: 7
                    },
                    NativeGroupKey {
                        artifact: leaf.descriptor.id,
                        original_ordinal: 7
                    },
                ])
            );
            assert!(demanded.artifact_ids().contains(&new.descriptor.id));
            drop(demanded);
            drop(branch);
            drop(retained);
            assert_eq!(inventory.node_count(), 0);
            assert_eq!(destination.node_count(), 0);
        }
    }

    #[test]
    fn native_requirement_cannot_select_an_ambient_exact_native_key() {
        let inventory = ArtifactInventory::default();
        let empty = inventory.empty_view();
        let original =
            issued_native_groups("Original", vec![(0, Vec::new())], &[], &BTreeMap::new());
        let ambient = inventory.admit(&empty, vec![original.clone()]).unwrap();
        let consumer = issued_native_groups(
            "Consumer",
            vec![(0, vec![issued_source(&original, 0)])],
            &[&original],
            &BTreeMap::new(),
        );
        let types = ambient
            .interface_projection(&[original.descriptor.owner.clone()])
            .unwrap();
        let before = inventory.node_count();
        assert!(inventory.admit(&types, vec![consumer.clone()]).is_err());
        assert_eq!(inventory.node_count(), before);
        assert!(types
            .native_requirements_from_roots(&[NativeRequirementRoot::Group {
                artifact: original.descriptor.id,
                original_ordinal: 0,
            }])
            .is_err());
        let selected = inventory.admit(&ambient, vec![consumer]).unwrap();
        assert!(selected
            .dependencies()
            .iter()
            .any(|(_, target, dependency)| *target == original.descriptor.id
                && matches!(dependency, ArtifactDependency::NativeGroup { .. })));
    }

    #[test]
    fn later_native_admission_cannot_broaden_an_interface_capability() {
        let inventory = ArtifactInventory::default();
        let canonical = entry("Owner", &[]);
        let canonical_id = canonical.descriptor.id;
        let type_cap = inventory
            .admit(&inventory.empty_view(), vec![canonical])
            .unwrap();
        let native = native_entry("Owner", &[]);
        let native_id = native.descriptor.id;
        let execution_cap = inventory.admit(&type_cap, vec![native]).unwrap();
        assert_eq!(type_cap.artifact_ids(), vec![canonical_id]);
        assert!(type_cap.entries().iter().all(|entry| !entry.is_native()));
        assert!(type_cap.source_implementation_roles().is_empty());
        assert_eq!(
            execution_cap.source_implementation_roles(),
            BTreeMap::from([(module("Owner"), ArtifactKind::OriginalModule)])
        );
        assert!(execution_cap.artifact_ids().contains(&native_id));
        assert!(execution_cap.dependencies().contains(&(
            native_id,
            canonical_id,
            ArtifactDependency::Interface
        )));
        let projected = execution_cap
            .interface_projection(&[module("Owner")])
            .unwrap();
        assert_eq!(projected.artifact_ids(), vec![canonical_id]);
        assert!(projected.source_implementation_roles().is_empty());
        drop(execution_cap);
        assert_eq!(inventory.node_count(), 1);
        assert_eq!(type_cap.artifact_ids(), vec![canonical_id]);
    }

    #[test]
    fn same_module_in_distinct_units_admits_together_with_exact_dependencies() {
        let inventory = ArtifactInventory::default();
        let first = entry_in_unit("cohort-a", "EpochModule", vec![]);
        let first_owner = first.descriptor.owner.clone();
        let first_id = first.descriptor.id;
        let second = entry_in_unit("cohort-b", "EpochModule", vec![first_owner.clone()]);
        let second_owner = second.descriptor.owner.clone();
        let second_id = second.descriptor.id;
        let view = inventory
            .admit(&inventory.empty_view(), vec![first, second])
            .unwrap();
        assert_eq!(view.descriptors().len(), 2);
        let entries = view
            .entries_for_owners([first_owner.clone(), second_owner.clone()].into_iter())
            .unwrap();
        assert_eq!(entries[&first_owner].descriptor.id, first_id);
        assert_eq!(entries[&second_owner].descriptor.id, second_id);
        assert_eq!(
            view.dependencies(),
            vec![(second_id, first_id, ArtifactDependency::Interface)]
        );
        let selected = view.select_roots(vec![second_id]).unwrap();
        assert_eq!(selected.root_entries().len(), 1);
        assert_eq!(selected.root_entries()[0].descriptor.owner, second_owner);
        drop(view);
        assert_eq!(selected.descriptors().len(), 2);
        assert_eq!(inventory.node_count(), 2);
        drop(selected);
        assert_eq!(inventory.node_count(), 0);
    }

    fn interface_only_original(
        name: &str,
        required: &ArtifactDescriptor,
        seal: [u8; 32],
    ) -> ArtifactEntry {
        ArtifactEntry::canonical(crate::certified_products::fixture_module_interface(
            [2; 32],
            "unit",
            name,
            BTreeMap::from([(
                (required.owner.unit.clone(), required.owner.module.clone()),
                seal,
            )]),
        ))
    }

    #[test]
    fn certified_interface_only_owner_survives_its_originating_reader_and_reclaims_with_original() {
        let inventory = ArtifactInventory::default();
        let empty = inventory.empty_view();
        let private = entry("PrivateNominalOwner", &[]);
        let dependent = interface_only_original(
            "PublishedOriginal",
            &private.descriptor,
            private.descriptor.interface_sha256,
        );
        assert_eq!(
            dependent.requirements,
            vec![private.descriptor.owner.clone()]
        );
        assert!(dependent.native_requirements.is_empty());
        let published = dependent.descriptor.id;
        let originating_reader = inventory.admit(&empty, vec![private]).unwrap();
        let issued = inventory
            .admit(&originating_reader, vec![dependent])
            .unwrap();
        let retained = issued.select_roots(vec![published]).unwrap();
        drop(issued);
        drop(originating_reader);
        assert_eq!(inventory.node_count(), 2);
        assert_eq!(retained.descriptors().len(), 2);
        assert_eq!(retained.interface_dependencies().len(), 1);
        assert!(retained
            .native_requirements_from_roots(&[NativeRequirementRoot::AllGroups(published)])
            .is_err());
        drop(retained);
        assert_eq!(inventory.node_count(), 0);
    }

    #[test]
    fn recovery_cannot_drop_original_certified_interface_only_edges() {
        let private = entry("PrivateNominalOwner", &[]);
        let dependent = interface_only_original(
            "PublishedOriginal",
            &private.descriptor,
            private.descriptor.interface_sha256,
        );
        let descriptors = vec![private.descriptor.clone(), dependent.descriptor.clone()];
        let mut entries = vec![private, dependent];
        assert!(restore_recovery_interface_dependencies(&mut entries, &descriptors, &[]).is_err());
        let edges = vec![(
            descriptors[1].id,
            descriptors[0].id,
            ArtifactDependency::Interface,
        )];
        restore_recovery_interface_dependencies(&mut entries, &descriptors, &edges).unwrap();
        let inventory = ArtifactInventory::default();
        let recovered = inventory.admit(&inventory.empty_view(), entries).unwrap();
        assert_eq!(recovered.interface_dependencies(), edges);
    }

    #[test]
    fn certified_interface_only_owner_rejects_wrong_unit_changed_interface_and_producer_atomically()
    {
        for wrong_unit in [false, true] {
            let inventory = ArtifactInventory::default();
            let empty = inventory.empty_view();
            let private = entry("PrivateNominalOwner", &[]);
            let mut evidence = private.descriptor.clone();
            if wrong_unit {
                evidence.owner.unit = "another-cohort".into();
            }
            let seal = if wrong_unit {
                evidence.interface_sha256
            } else {
                [9; 32]
            };
            let dependent = interface_only_original("PublishedOriginal", &evidence, seal);
            let failure = inventory
                .admit(&empty, vec![private, dependent])
                .unwrap_err();
            let CompileError::ArtifactInventory(failure) = failure else {
                panic!("expected inventory refusal")
            };
            let failure = &failure.failure;
            assert!(if wrong_unit {
                matches!(failure, ArtifactInventoryFailure::MissingDependency { .. })
            } else {
                matches!(
                    failure,
                    ArtifactInventoryFailure::InterfaceSealMismatch { .. }
                )
            });
            assert_eq!(inventory.node_count(), 0);
        }
        let inventory = ArtifactInventory::default();
        let empty = inventory.empty_view();
        let mut private = entry("PrivateNominalOwner", &[]);
        let dependent = interface_only_original(
            "PublishedOriginal",
            &private.descriptor,
            private.descriptor.interface_sha256,
        );
        private = ArtifactEntry::canonical(crate::certified_products::fixture_module_interface(
            [8; 32],
            "unit",
            "PrivateNominalOwner",
            BTreeMap::new(),
        ));
        assert!(inventory.admit(&empty, vec![private, dependent]).is_err());
        assert_eq!(inventory.node_count(), 0);
    }

    #[test]
    fn owner_conflict_retains_exact_pair_after_unrelated_view_is_reclaimed() {
        let inventory = ArtifactInventory::default();
        let empty = inventory.empty_view();
        let existing =
            ArtifactEntry::canonical(crate::certified_products::fixture_module_interface(
                [1; 32],
                "unit",
                "PrivateOwner",
                BTreeMap::new(),
            ));
        let incoming =
            ArtifactEntry::canonical(crate::certified_products::fixture_module_interface(
                [2; 32],
                "unit",
                "PrivateOwner",
                BTreeMap::new(),
            ));
        let retained = inventory.admit(&empty, vec![existing.clone()]).unwrap();
        let CompileError::ArtifactInventory(error) = inventory
            .admit(&retained, vec![incoming.clone()])
            .unwrap_err()
        else {
            panic!("expected exact owner conflict")
        };
        drop(retained);
        assert_eq!(inventory.node_count(), 0);
        let destination = tempfile::tempdir().unwrap();
        error.retain_owner_conflict(destination.path()).unwrap();
        let root = destination.path().join("owner-conflict");
        let index: serde_json::Value =
            serde_json::from_slice(&std::fs::read(root.join("index.json")).unwrap()).unwrap();
        assert_eq!(index["boundary"], "artifact_selection");
        assert_eq!(index["existing_in_parent"], true);
        for (label, entry) in [("existing", existing), ("incoming", incoming)] {
            let ArtifactPayload::Canonical(interface) = entry.payload else {
                unreachable!()
            };
            assert_eq!(
                std::fs::read(root.join(format!("{label}.finalized.cbor"))).unwrap(),
                interface.certificate_bytes()
            );
            assert_eq!(
                index[label],
                serde_json::to_value(entry.descriptor).unwrap()
            );
        }
    }

    #[test]
    fn compiler_role_conflict_reports_bounded_exact_ids_and_retains_full_selection() {
        let owner = ExactModuleIdentity {
            unit: "unit".into(),
            module: "Shared".into(),
        };
        let existing = CompilerInputRole::InterfaceOnly {
            interface: ArtifactId([1; 32]),
        };
        let incoming = CompilerInputRole::PublishedSourceOriginal {
            interface: ArtifactId([2; 32]),
            original: ArtifactId([3; 32]),
            source_revision: "source".into(),
            original_input_identity: "input".into(),
            selection: ExactArtifactSelection::from_recovered_graph_selection(
                ArtifactGraphSelection {
                    selections: vec![],
                    bindings: vec![
                        ArtifactBinding {
                            artifact: ArtifactId([4; 32]),
                            selection: ArtifactSelectionId([6; 32])
                        };
                        10_000
                    ],
                    namespace: vec![],
                    roots: vec![],
                    native_groups: vec![],
                    custody_roots: vec![],
                },
            ),
            selection_sha256: [5; 32],
        };
        let mut projection = CompilerInputProjection::default();
        projection
            .admit_role(owner.clone(), existing.clone())
            .unwrap();
        let CompileError::ArtifactInventory(error) =
            projection.admit_role(owner, incoming.clone()).unwrap_err()
        else {
            panic!("expected compiler role refusal")
        };
        assert_eq!(projection.roles(), vec![existing]);
        for rendered in [error.to_string(), format!("{error:?}")] {
            assert!(rendered.len() < 1024);
            assert!(rendered.contains("compiler_role"));
            assert!(rendered.contains(&"02".repeat(32)));
            assert!(!rendered.contains("selection_sha256"));
        }
        let destination = tempfile::tempdir().unwrap();
        error.retain_owner_conflict(destination.path()).unwrap();
        let index: serde_json::Value = serde_json::from_slice(
            &std::fs::read(destination.path().join("owner-conflict/index.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(index["boundary"], "compiler_role");
        assert_eq!(
            index["incoming_role"],
            serde_json::to_value(incoming).unwrap()
        );
    }

    #[test]
    fn native_canonical_conflict_reports_selected_and_required_carriers() {
        let native = native_entry("Shared", &[]);
        let selected = Arc::new(ArtifactEntry::canonical(
            crate::certified_products::fixture_module_interface(
                [3; 32],
                "unit",
                "Shared",
                BTreeMap::new(),
            ),
        ));
        let entries = BTreeMap::from([(selected.descriptor.id, Arc::clone(&selected))]);
        let owners = SelectedOwners {
            interfaces: BTreeMap::from([(
                selected.descriptor.owner.clone(),
                selected.descriptor.id,
            )]),
            native: BTreeMap::new(),
        };
        let inventory = ArtifactInventory::default();
        let state = inventory.0.lock().unwrap();
        let CompileError::ArtifactInventory(error) =
            owners.dependencies(&state, &native, &entries).unwrap_err()
        else {
            panic!("expected native canonical refusal")
        };
        let evidence = error.owner_conflict.as_ref().unwrap().metadata();
        assert_eq!(evidence["boundary"], "native_canonical_carrier");
        assert_eq!(
            evidence["existing"],
            serde_json::to_value(&selected.descriptor).unwrap()
        );
        assert_ne!(evidence["incoming"]["id"], evidence["existing"]["id"]);
        assert!(error.to_string().contains("native_canonical_carrier"));
        assert_eq!(state.graph.node_count(), 0);
    }

    #[test]
    fn same_module_in_distinct_units_admits_successively_and_reclaims_independently() {
        let inventory = ArtifactInventory::default();
        let empty = inventory.empty_view();
        let first_entry = entry_in_unit("cohort-a", "EpochModule", vec![]);
        let first_owner = first_entry.descriptor.owner.clone();
        let second_entry = entry_in_unit("cohort-b", "EpochModule", vec![]);
        let second_owner = second_entry.descriptor.owner.clone();
        let first = inventory.admit(&empty, vec![first_entry.clone()]).unwrap();
        let captured = first.clone();
        let second = inventory.admit(&first, vec![second_entry]).unwrap();
        assert_eq!(second.descriptors().len(), 2);
        assert_eq!(
            first
                .entries_for_owners([first_owner.clone(), second_owner].into_iter())
                .unwrap()
                .keys()
                .collect::<Vec<_>>(),
            vec![&first_owner],
            "retaining a same-named foreign unit does not expose it through an older view"
        );
        drop(second);
        assert_eq!(inventory.node_count(), 1);
        drop(first);
        assert_eq!(inventory.node_count(), 1);
        assert_eq!(captured.descriptors()[0].owner, first_owner);
        drop(captured);
        assert_eq!(inventory.node_count(), 0);
        let readmitted = inventory.admit(&empty, vec![first_entry]).unwrap();
        assert_eq!(readmitted.descriptors()[0].owner, first_owner);
        drop(readmitted);
        assert_eq!(inventory.node_count(), 0);
    }

    #[test]
    fn disjoint_exact_owner_variants_survive_and_reclaim_independently() {
        let inventory = ArtifactInventory::default();
        let empty = inventory.empty_view();
        let old = entry("Shared", &[]);
        let new = ArtifactEntry::canonical(crate::certified_products::fixture_module_interface(
            [3; 32],
            "unit",
            "Shared",
            BTreeMap::new(),
        ));
        let old_view = inventory.admit(&empty, vec![old.clone()]).unwrap();
        let new_view = inventory.admit(&empty, vec![new.clone()]).unwrap();
        assert_eq!(old_view.descriptors(), vec![old.descriptor.clone()]);
        assert_eq!(new_view.descriptors(), vec![new.descriptor.clone()]);
        assert_eq!(inventory.node_count(), 2);
        let before = inventory.node_count();
        assert!(old_view.merge(&new_view).is_err());
        assert_eq!(inventory.node_count(), before);
        let duplicate = inventory.admit(&empty, vec![old]).unwrap();
        drop(old_view);
        assert_eq!(inventory.node_count(), 2);
        drop(duplicate);
        assert_eq!(inventory.node_count(), 1);
        assert_eq!(new_view.descriptors(), vec![new.descriptor]);
        drop(new_view);
        assert_eq!(inventory.node_count(), 0);
    }

    #[test]
    fn cross_inventory_conflicting_merge_is_atomic() {
        let first = ArtifactInventory::default();
        let second = ArtifactInventory::default();
        let old = first
            .admit(&first.empty_view(), vec![entry("Shared", &[])])
            .unwrap();
        let new = second
            .admit(
                &second.empty_view(),
                vec![
                    ArtifactEntry::canonical(crate::certified_products::fixture_module_interface(
                        [3; 32],
                        "unit",
                        "Shared",
                        BTreeMap::new(),
                    )),
                    entry("Fresh", &[]),
                ],
            )
            .unwrap();
        let old_ids = old.artifact_ids();
        let new_ids = new.artifact_ids();
        assert!(old.merge(&new).is_err());
        assert_eq!(first.node_count(), 1);
        assert_eq!(second.node_count(), 2);
        assert_eq!(old.artifact_ids(), old_ids);
        assert_eq!(new.artifact_ids(), new_ids);
    }

    #[test]
    fn ambient_owner_cannot_satisfy_a_dependency_or_missing_seal() {
        let inventory = ArtifactInventory::default();
        let empty = inventory.empty_view();
        let ambient = inventory
            .admit(&empty, vec![entry("Support", &[])])
            .unwrap();
        let dependent = entry("Consumer", &["Support"]);
        assert!(matches!(inventory.admit(&empty, vec![dependent.clone()]),
            Err(CompileError::ArtifactInventory(error)) if matches!(error.failure, ArtifactInventoryFailure::MissingDependency { .. })));
        assert_eq!(inventory.node_count(), 1);
        for drop_requirements in [false, true] {
            let mut unsealed = dependent.clone();
            unsealed.interface_seals.clear();
            if drop_requirements {
                unsealed.requirements.clear();
            }
            assert!(inventory.admit(&ambient, vec![unsealed]).is_err());
            assert_eq!(inventory.node_count(), 1);
        }
        let selected = inventory.admit(&ambient, vec![dependent]).unwrap();
        assert_eq!(selected.descriptors().len(), 2);
    }

    #[test]
    fn reused_artifact_cannot_hide_a_conflicting_dependency_variant() {
        let inventory = ArtifactInventory::default();
        let empty = inventory.empty_view();
        let consumer = entry("Consumer", &["Support"]);
        let retained = inventory
            .admit(&empty, vec![consumer.clone(), entry("Support", &[])])
            .unwrap();
        let replacement =
            ArtifactEntry::canonical(crate::certified_products::fixture_module_interface(
                [3; 32],
                "unit",
                "Support",
                BTreeMap::new(),
            ));
        assert!(inventory
            .admit(&empty, vec![consumer, replacement, entry("Fresh", &[])])
            .is_err());
        assert_eq!(inventory.node_count(), 2);
        assert_eq!(retained.descriptors().len(), 2);
    }

    #[test]
    fn reused_graph_targets_must_match_the_selected_sealed_dependencies() {
        let inventory = ArtifactInventory::default();
        let empty = inventory.empty_view();
        let consumer = entry("Consumer", &["Support"]);
        let retained = inventory
            .admit(&empty, vec![consumer.clone(), entry("Support", &[])])
            .unwrap();
        let unrelated = inventory
            .admit(&empty, vec![entry("Unrelated", &[])])
            .unwrap();
        let state = inventory.0.lock().unwrap();
        let source = InventoryNodeKey::Artifact(
            retained
                .resolve_binding(
                    consumer.descriptor.id,
                    &retained.read_projection(&state).nodes,
                )
                .unwrap(),
        );
        let target = unrelated.lease.roots[0];
        let mut edges = state
            .graph
            .edges(state.indices[&source])
            .map(|edge| (state.graph[edge.target()], edge.weight().clone()))
            .collect::<BTreeSet<_>>();
        edges.insert((target, ArtifactDependency::Interface));
        let plan = BTreeMap::from([(source, edges)]);
        let entries = state.payloads.clone();
        assert!(
            matches!(BindingCommit::prepare(&state, &entries, &plan), Err(CompileError::ArtifactInventory(error)) if matches!(error.failure, ArtifactInventoryFailure::MetadataConflict { artifact } if artifact == consumer.descriptor.id))
        );
        drop(state);
        assert_eq!(inventory.node_count(), 3);
        drop(retained);
        drop(unrelated);
    }

    #[test]
    fn exact_owner_conflicts_are_atomic_in_one_batch_and_later_admission() {
        for simultaneous in [true, false] {
            let inventory = ArtifactInventory::default();
            let empty = inventory.empty_view();
            let original = entry_in_unit("cohort-a", "EpochModule", vec![]);
            let owner = original.descriptor.owner.clone();
            let conflicting =
                ArtifactEntry::canonical(crate::certified_products::fixture_module_interface(
                    [3; 32],
                    "cohort-a",
                    "EpochModule",
                    BTreeMap::new(),
                ));
            let retained = if simultaneous {
                empty.clone()
            } else {
                inventory.admit(&empty, vec![original.clone()]).unwrap()
            };
            let mut incoming = vec![conflicting, entry("Unrelated", &[])];
            if simultaneous {
                incoming.push(original.clone());
            }
            let failure = inventory.admit(&retained, incoming).unwrap_err();
            assert!(matches!(failure, CompileError::ArtifactInventory(error)
                if error.failure == ArtifactInventoryFailure::OwnerConflict { owner: owner.clone() }));
            assert_eq!(inventory.node_count(), usize::from(!simultaneous));
            if !simultaneous {
                assert_eq!(retained.entries()[0].as_ref(), &original);
                let duplicate = inventory.admit(&retained, vec![original]).unwrap();
                assert_eq!(duplicate.descriptors().len(), 1);
                drop(duplicate);
            }
            drop(retained);
            assert_eq!(inventory.node_count(), 0);
        }
    }

    #[test]
    fn same_module_in_another_unit_does_not_satisfy_missing_exact_dependency() {
        let inventory = ArtifactInventory::default();
        let retained = inventory
            .admit(
                &inventory.empty_view(),
                vec![entry_in_unit("cohort-b", "EpochModule", vec![])],
            )
            .unwrap();
        let missing = ExactModuleIdentity {
            unit: "cohort-a".into(),
            module: "EpochModule".into(),
        };
        let failure = inventory
            .admit(
                &retained,
                vec![entry_in_unit("main", "Consumer", vec![missing.clone()])],
            )
            .unwrap_err();
        assert!(matches!(failure, CompileError::ArtifactInventory(error)
            if matches!(&error.failure, ArtifactInventoryFailure::MissingDependency { required, .. }
                if required == &missing)));
        assert_eq!(inventory.node_count(), 1);
    }
    #[test]
    fn retained_views_reclaim_after_last_reader_without_copying_payloads() {
        let inventory = ArtifactInventory::default();
        let empty = inventory.empty_view();
        let first = inventory
            .admit(&empty, vec![entry("Original", &[])])
            .unwrap();
        let capture = first.clone();
        let second = inventory
            .admit(&first, vec![entry("Consumer", &["Original"])])
            .unwrap();
        assert_eq!(second.dependencies().len(), 1);
        let original = first.entries()[0].clone();
        let inherited = second
            .entries()
            .into_iter()
            .find(|entry| entry.descriptor.owner == module("Original"))
            .unwrap();
        assert!(Arc::ptr_eq(&original, &inherited));
        drop(second);
        assert_eq!(inventory.node_count(), 1);
        drop(first);
        assert_eq!(inventory.node_count(), 1);
        drop(capture);
        assert_eq!(inventory.node_count(), 0);
        // The temporary inspection owner may retain bytes, never graph authority.
        assert_eq!(original.descriptor.owner, module("Original"));
    }
    #[test]
    fn cross_inventory_merge_keeps_hidden_dependencies_out_of_selected_roots() {
        let destination = ArtifactInventory::default();
        let existing = destination
            .admit(&destination.empty_view(), vec![entry("Existing", &[])])
            .unwrap();
        let source = ArtifactInventory::default();
        let support = entry("Support", &["Hidden"]);
        let support_id = support.descriptor.id;
        let history = source
            .admit(
                &source.empty_view(),
                vec![support, entry("Hidden", &[]), entry("Unrelated", &[])],
            )
            .unwrap();
        let selected = history.select_roots(vec![support_id]).unwrap();
        let merged = existing.merge(&selected).unwrap();
        let mut roots = merged
            .root_entries()
            .iter()
            .map(|entry| entry.descriptor.owner.module.clone())
            .collect::<Vec<_>>();
        roots.sort();
        assert_eq!(roots, ["Existing", "Support"]);
        assert_eq!(merged.descriptors().len(), 3);
        assert_eq!(merged.dependencies().len(), 1);
        drop(existing);
        drop(selected);
        drop(history);
        assert_eq!(source.node_count(), 0);
        assert_eq!(destination.node_count(), 3);
        drop(merged);
        assert_eq!(destination.node_count(), 0);
    }
    #[test]
    fn reclamation_metrics_follow_last_reader_and_selected_closure() {
        let inventory = ArtifactInventory::default();
        let empty = inventory.empty_view();
        let original = inventory
            .admit(&empty, vec![entry("Original", &[])])
            .unwrap();
        let retained = original.clone();
        let dependent = inventory
            .admit(&original, vec![entry("Consumer", &["Original"])])
            .unwrap();
        let before = inventory.metrics();
        drop(dependent);
        let after = inventory.metrics();
        assert_eq!(after.reclamation_runs - before.reclamation_runs, 1);
        assert_eq!(
            after.reclamation_candidate_nodes - before.reclamation_candidate_nodes,
            2
        );
        assert_eq!(after.reclaimed_nodes - before.reclaimed_nodes, 1);
        assert_eq!(after.nodes, 1);
        drop(original);
        assert_eq!(
            inventory.metrics(),
            after,
            "an Arc reader drop performs no graph reclamation"
        );
        drop(retained);
        let final_metrics = inventory.metrics();
        assert_eq!(final_metrics.reclamation_runs - before.reclamation_runs, 2);
        assert_eq!(
            final_metrics.reclamation_candidate_nodes - before.reclamation_candidate_nodes,
            3
        );
        assert_eq!(final_metrics.reclaimed_nodes - before.reclaimed_nodes, 2);
        assert_eq!(final_metrics.nodes, 0);
        let encoded = serde_json::to_value(final_metrics).unwrap();
        assert!(encoded.get("whole_graph_copies").is_none());
        assert_eq!(encoded["structural_whole_graph_copies"], 0);
    }

    #[test]
    fn empty_admission_and_parent_only_release_do_not_scan_history() {
        let inventory = ArtifactInventory::default();
        let empty = inventory.empty_view();
        let view = inventory
            .admit(&empty, vec![entry("Original", &[])])
            .unwrap();
        let before = inventory.metrics();
        let unchanged = inventory.admit(&view, Vec::new()).unwrap();
        assert!(Arc::ptr_eq(&view.lease, &unchanged.lease));
        let merged = view.merge(&unchanged).unwrap();
        drop(merged);
        drop(unchanged);
        let after = inventory.metrics();
        assert_eq!(before.graph_visits, after.graph_visits);
        assert_eq!(
            before.admission_owner_lookups,
            after.admission_owner_lookups
        );
        let second = inventory
            .admit(&view, vec![entry("Consumer", &["Original"])])
            .unwrap();
        assert_eq!(
            inventory.metrics().admission_owner_lookups - after.admission_owner_lookups,
            4
        );
        drop(second);
        drop(view);
        assert_eq!(inventory.node_count(), 0);
        // Reclamation removes the immutable ID registry entries with the graph nodes.
        let replacement = inventory
            .admit(&empty, vec![entry("Original", &[])])
            .unwrap();
        assert_eq!(replacement.descriptors().len(), 1);
    }

    #[test]
    fn artifact_subset_materialization_preserves_selected_custody_and_bounded_warm_reads() {
        let inventory = ArtifactInventory::default();
        let mut native = issued_native_groups("Root", vec![(7, vec![])], &[], &BTreeMap::new());
        let ArtifactPayload::Original(product) = &native.payload else {
            unreachable!()
        };
        native.payload = ArtifactPayload::Original(product.clone().with_source_sha256([1; 32]));
        let id = native.descriptor.id;
        assert!(native.source_admission_witness().is_some());
        let unused = entry_in_unit("fixture", "Unused", vec![]);
        let unused_id = unused.descriptor.id;
        let mut supplied = (0..128)
            .map(|ordinal| entry_in_unit("fixture", &format!("Extra{ordinal}"), vec![]))
            .collect::<Vec<_>>();
        supplied.extend([native, unused]);
        let broad = inventory.admit(&inventory.empty_view(), supplied).unwrap();
        let selected = broad.select_roots(vec![id]).unwrap();
        assert!(selected.reads.read_projection.get().is_none());
        let before = inventory.metrics();
        assert!(selected
            .entries_for_artifacts(&BTreeSet::new())
            .unwrap()
            .is_empty());
        assert!(selected.reads.read_projection.get().is_none());
        assert_eq!(inventory.metrics(), before);

        let broad_entries = broad.entries();
        let selected_entries = selected.entries();
        let before = inventory.metrics();
        for (view, expected) in [(&broad, &broad_entries), (&selected, &selected_entries)] {
            let entries = view.entries_for_artifacts(&BTreeSet::from([id])).unwrap();
            assert_eq!(entries.len(), 1);
            let issued = expected
                .iter()
                .find(|entry| entry.descriptor.id == id)
                .unwrap();
            assert!(Arc::ptr_eq(&entries[0], issued));
            assert_eq!(
                entries[0].source_admission_witness(),
                issued.source_admission_witness()
            );
        }
        let after = inventory.metrics();
        assert_eq!(after.graph_visits, before.graph_visits);
        assert_eq!(after.entry_handle_copies - before.entry_handle_copies, 2);
        assert!(inventory.0.lock().unwrap().payloads[&id]
            .source_admission_witness()
            .is_none());
        assert!(selected
            .selection_snapshot()
            .scopes
            .values()
            .any(|reference| reference
                .proof()
                .nodes
                .iter()
                .any(|node| node.artifact() == unused_id)));
        assert!(inventory
            .0
            .lock()
            .unwrap()
            .payloads
            .contains_key(&unused_id));
        assert!(selected
            .entries_for_artifacts(&BTreeSet::from([unused_id]))
            .is_err());
        assert!(selected
            .entries_for_artifacts(&BTreeSet::from([ArtifactId([0xee; 32])]))
            .is_err());
        drop(broad);
        assert!(!inventory
            .0
            .lock()
            .unwrap()
            .payloads
            .contains_key(&unused_id));
        assert!(selected
            .entries_for_artifacts(&BTreeSet::from([unused_id]))
            .is_err());
    }

    #[test]
    fn selected_and_retained_reads_share_one_closure_without_custody_roots() {
        for count in [1, 8, 32] {
            for selected_first in [false, true] {
                let inventory = ArtifactInventory::default();
                let original = issued_native_groups(
                    "Original",
                    (0..count).map(|ordinal| (ordinal, Vec::new())).collect(),
                    &[],
                    &BTreeMap::new(),
                );
                let view = inventory
                    .admit(&inventory.empty_view(), vec![original])
                    .unwrap();
                assert!(view.custody_roots().is_empty());
                let before = inventory.metrics().graph_visits;
                if selected_first {
                    assert_eq!(view.selected_native_groups().len(), count as usize);
                    view.metadata_snapshot();
                } else {
                    view.metadata_snapshot();
                    assert_eq!(view.selected_native_groups().len(), count as usize);
                }
                let after = inventory.metrics().graph_visits;
                assert_eq!(after - before, u64::from(count) + 2);
                for _ in 0..3 {
                    view.metadata_snapshot();
                    assert_eq!(view.selected_native_groups().len(), count as usize);
                }
                assert_eq!(inventory.metrics().graph_visits, after);
            }
        }
    }

    #[test]
    fn metadata_snapshot_preserves_canonical_typed_edges_in_one_closure() {
        let inventory = ArtifactInventory::default();
        let original =
            issued_native_groups("Original", vec![(7, Vec::new())], &[], &BTreeMap::new());
        let consumer = issued_native_groups(
            "Consumer",
            vec![(
                4,
                vec![
                    issued_source(&original, 7),
                    crate::certified_products::PendingImportOwner::Retained {
                        identity: tidepool_repr::execution_schema::SymbolIdentity {
                            unit: original.descriptor.owner.unit.clone(),
                            module: "Original".into(),
                            namespace: "value".into(),
                            occurrence: "kept".into(),
                            record_parent: None,
                        },
                        generation: 9,
                    },
                ],
            )],
            &[&original],
            &BTreeMap::new(),
        );
        let view = inventory
            .admit(&inventory.empty_view(), vec![consumer, original])
            .unwrap();
        let expected_descriptors = view.descriptors();
        let expected_dependencies = view.dependencies();
        let before = inventory.metrics();
        let metadata = view.metadata_snapshot();
        assert_eq!(
            metadata
                .descriptors()
                .into_iter()
                .cloned()
                .collect::<Vec<_>>(),
            expected_descriptors,
        );
        assert_eq!(metadata.dependencies(), expected_dependencies);
        let after = inventory.metrics();
        assert_eq!(after.graph_visits - before.graph_visits, 0);
        assert_eq!(after.view_queries - before.view_queries, 1);
        assert_eq!(after.entry_handle_copies - before.entry_handle_copies, 6);
    }

    #[test]
    fn reclamation_preserves_incoming_cycles_and_does_not_visit_unrelated_history() {
        let inventory = ArtifactInventory::default();
        let empty = inventory.empty_view();
        let cycle = inventory
            .admit(&empty, vec![entry("A", &["B"]), entry("B", &["A"])])
            .unwrap();
        let incoming = inventory
            .admit(&cycle, vec![entry("Outside", &["A"])])
            .unwrap();
        let unrelated = inventory
            .admit(&empty, vec![entry("Unrelated", &[])])
            .unwrap();
        let before = inventory.metrics().graph_visits;
        drop(cycle);
        assert_eq!(inventory.node_count(), 4);
        assert_eq!(inventory.metrics().graph_visits - before, 0);
        drop(incoming);
        assert_eq!(inventory.node_count(), 1);
        assert_eq!(unrelated.descriptors()[0].owner.module, "Unrelated");
        drop(unrelated);
        assert_eq!(inventory.node_count(), 0);
    }

    #[test]
    fn native_binding_selection_ignores_type_edges_and_unselected_group_ordinals() {
        let inventory = ArtifactInventory::default();
        let empty = inventory.empty_view();
        let value = entry("Val", &[]);
        let type_user = entry("TypeOnly", &["Val"]);
        let capture = |occurrence: &str, generation| {
            crate::certified_products::PendingImportOwner::Retained {
                identity: tidepool_repr::execution_schema::SymbolIdentity {
                    unit: value.descriptor.owner.unit.clone(),
                    module: "Val".into(),
                    namespace: "value".into(),
                    occurrence: occurrence.into(),
                    record_parent: None,
                },
                generation,
            }
        };
        let helper = issued_native_groups(
            "Helper",
            vec![(1, vec![capture("x", 7)]), (2, vec![capture("y", 8)])],
            &[&value],
            &BTreeMap::new(),
        );
        let consumer = issued_native_groups(
            "Consumer",
            vec![(0, vec![issued_source(&helper, 1)])],
            &[&type_user, &helper],
            &BTreeMap::new(),
        );
        let type_id = type_user.descriptor.id;
        let helper_id = helper.descriptor.id;
        let consumer_id = consumer.descriptor.id;
        let view = inventory
            .admit(&empty, vec![value, type_user, helper, consumer])
            .unwrap();
        assert!(view
            .native_binding_requirements_from_roots(&[NativeRequirementRoot::AllGroups(type_id)])
            .is_err());
        let selected = view
            .native_binding_requirements_from_roots(&[NativeRequirementRoot::AllGroups(
                consumer_id,
            )])
            .unwrap();
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].identity.module, "Val");
        assert_eq!(selected[0].identity.occurrence, "x");
        assert_eq!(selected[0].generation, 7);
        assert_eq!(
            view.native_binding_requirements_from_roots(&[NativeRequirementRoot::AllGroups(
                helper_id
            )])
            .unwrap()
            .len(),
            2
        );
        assert!(view
            .native_binding_requirements_from_roots(&[NativeRequirementRoot::AllGroups(
                ArtifactId([99; 32])
            )])
            .is_err());
    }

    #[test]
    fn conflicting_owner_and_missing_dependency_admit_nothing() {
        let inventory = ArtifactInventory::default();
        let empty = inventory.empty_view();
        let first = inventory
            .admit(&empty, vec![entry("Original", &[])])
            .unwrap();
        let mut changed = entry("Original", &[]);
        changed.descriptor.id = ArtifactId([9; 32]);
        assert!(inventory
            .admit(&first, vec![entry("Unrelated", &[]), changed])
            .is_err());
        assert_eq!(inventory.node_count(), 1);
        assert!(inventory
            .admit(&first, vec![entry("Missing", &["Absent"])])
            .is_err());
        assert_eq!(inventory.node_count(), 1);
    }
    #[test]
    fn independent_views_retain_existing_nodes_without_sibling_visibility() {
        let inventory = ArtifactInventory::default();
        let empty = inventory.empty_view();
        let first = inventory.admit(&empty, vec![entry("First", &[])]).unwrap();
        let second = inventory.admit(&empty, vec![entry("Second", &[])]).unwrap();
        assert_eq!(
            second
                .descriptors()
                .iter()
                .map(|descriptor| descriptor.owner.module.as_str())
                .collect::<Vec<_>>(),
            vec!["Second"]
        );
        let imported = inventory.admit(&second, vec![entry("First", &[])]).unwrap();
        drop(first);
        assert_eq!(imported.descriptors().len(), 2);
        drop(imported);
        assert_eq!(inventory.node_count(), 1);
        drop(second);
        assert_eq!(inventory.node_count(), 0);
    }
    #[test]
    fn selected_root_keeps_hidden_requirements_and_reclaims_unselected_history() {
        let inventory = ArtifactInventory::default();
        let empty = inventory.empty_view();
        let view = inventory
            .admit(
                &empty,
                vec![
                    entry("Hidden", &[]),
                    entry("Visible", &["Hidden"]),
                    entry("Obsolete", &[]),
                ],
            )
            .unwrap();
        let visible = view
            .descriptors()
            .into_iter()
            .find(|descriptor| descriptor.owner == module("Visible"))
            .unwrap()
            .id;
        let selected = view.select_roots(vec![visible]).unwrap();
        assert!(selected.select_roots(vec![ArtifactId([99; 32])]).is_err());
        drop(view);
        assert_eq!(inventory.node_count(), 2);
        assert_eq!(selected.descriptors().len(), 2);
        let metrics = inventory.metrics();
        assert_eq!(metrics.whole_graph_copies, 0);
        assert!(metrics.graph_visits > 0);
        assert!(metrics.entry_handle_copies > 0);
        drop(selected);
        assert_eq!(inventory.node_count(), 0);
    }
    #[test]
    fn native_requirements_remain_typed_and_do_not_grant_a_value_implementation() {
        let inventory = ArtifactInventory::default();
        let empty = inventory.empty_view();
        let original =
            issued_native_groups("Original", vec![(7, Vec::new())], &[], &BTreeMap::new());
        let consumer = issued_native_groups(
            "Consumer",
            vec![(2, vec![issued_source(&original, 7)])],
            &[&original],
            &BTreeMap::new(),
        );
        let view = inventory.admit(&empty, vec![original, consumer]).unwrap();
        let dependencies = view.dependencies();
        assert_eq!(dependencies.len(), 4);
        assert!(dependencies
            .iter()
            .any(|(_, _, edge)| matches!(edge, ArtifactDependency::Interface)));
        assert!(dependencies.iter().any(|(_, _, edge)| matches!(
            edge,
            ArtifactDependency::NativeGroup {
                dependent_ordinal: 2,
                required_ordinal: 7
            }
        )));
        assert_eq!(
            view.interface_dependencies(),
            dependencies
                .into_iter()
                .filter(|(_, _, dependency)| matches!(dependency, ArtifactDependency::Interface))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn recovery_restores_interface_edges_and_keeps_certified_native_facts() {
        let original =
            issued_native_groups("Original", vec![(7, Vec::new())], &[], &BTreeMap::new());
        let consumer = issued_native_groups(
            "Consumer",
            vec![(2, vec![issued_source(&original, 7)])],
            &[&original],
            &BTreeMap::new(),
        );
        let canonical = |entry: &ArtifactEntry| {
            let ArtifactPayload::Original(product) = &entry.payload else {
                panic!("native fixture")
            };
            ArtifactEntry::canonical(product.module_interface().unwrap().clone())
        };
        let original_interface = canonical(&original);
        let consumer_interface = canonical(&consumer);
        let edges = vec![
            (
                original.descriptor.id,
                original_interface.descriptor.id,
                ArtifactDependency::Interface,
            ),
            (
                consumer.descriptor.id,
                consumer_interface.descriptor.id,
                ArtifactDependency::Interface,
            ),
            (
                consumer_interface.descriptor.id,
                original_interface.descriptor.id,
                ArtifactDependency::Interface,
            ),
        ];
        let mut entries = vec![
            original.clone(),
            consumer.clone(),
            original_interface,
            consumer_interface,
        ];
        let descriptors = entries
            .iter()
            .map(|entry| entry.descriptor.clone())
            .collect::<Vec<_>>();
        restore_recovery_interface_dependencies(&mut entries, &descriptors, &edges).unwrap();
        assert_eq!(
            entries[1].requirements,
            vec![original.descriptor.owner.clone()]
        );
        assert_eq!(entries[1].native_requirements, consumer.native_requirements);
        let inventory = ArtifactInventory::default();
        let view = inventory
            .admit(&inventory.empty_view(), entries.clone())
            .unwrap();
        assert!(view.dependencies().iter().any(|(_, _, edge)| matches!(
            edge,
            ArtifactDependency::NativeGroup {
                dependent_ordinal: 2,
                required_ordinal: 7
            }
        )));
        let mut supplied_native = edges.clone();
        supplied_native.push((
            consumer.descriptor.id,
            original.descriptor.id,
            consumer.native_requirements[&2][0].1.clone(),
        ));
        assert!(restore_recovery_interface_dependencies(
            &mut entries,
            &descriptors,
            &supplied_native
        )
        .is_err());
        let mut forged = descriptors.clone();
        forged[0].producer_sha256 = [99; 32];
        assert!(restore_recovery_interface_dependencies(&mut entries, &forged, &edges).is_err());
        let mut duplicated = edges.clone();
        duplicated.push(edges[0].clone());
        assert!(
            restore_recovery_interface_dependencies(&mut entries, &descriptors, &duplicated)
                .is_err()
        );
        let mut wrong_endpoint = edges;
        wrong_endpoint[0].1 = ArtifactId([99; 32]);
        assert!(restore_recovery_interface_dependencies(
            &mut entries,
            &descriptors,
            &wrong_endpoint
        )
        .is_err());
    }

    #[test]
    fn dependency_and_artifact_ids_are_stable_across_inventory_allocation() {
        let a = ArtifactInventory::default();
        let b = ArtifactInventory::default();
        let av = a
            .admit(
                &a.empty_view(),
                vec![entry("Original", &[]), entry("Consumer", &["Original"])],
            )
            .unwrap();
        let bv = b
            .admit(
                &b.empty_view(),
                vec![entry("Consumer", &["Original"]), entry("Original", &[])],
            )
            .unwrap();
        assert_eq!(av.artifact_ids(), bv.artifact_ids());
        assert_eq!(av.dependencies(), bv.dependencies());
        let json = serde_json::to_string(&av.descriptors()).unwrap();
        assert!(!json.contains("NodeIndex"));
    }
    #[test]
    fn selected_initial_group_excludes_later_captures_and_accepts_issued_empty_group() {
        use crate::certified_products::{PackageInterfaceWitness, PendingImportOwner};
        use tidepool_repr::execution_schema::testing;

        let package_file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(package_file.path(), b"selected package interface").unwrap();
        let package_digest = digest(b"selected package interface");
        let package_identity = |occurrence: &str| {
            let mut identity = testing::identity("Package", occurrence);
            identity.unit = "package-unit".into();
            identity
        };
        let packages = BTreeMap::from([(
            ("package-unit".into(), "Package".into()),
            PackageInterfaceWitness {
                selected_path: package_file.path().to_path_buf(),
                sha256: package_digest,
            },
        )]);
        let capture = |occurrence: &str, generation| PendingImportOwner::Retained {
            identity: testing::identity("Values", occurrence),
            generation,
        };
        let package = |occurrence: &str, generation| PendingImportOwner::RetainedPackage {
            unit: "package-unit".into(),
            module: "Package".into(),
            binder: package_identity(occurrence),
            generation,
            interface_digest: package_digest,
        };
        let product = crate::certified_products::tests::original_groups_fixture(
            "Segment",
            vec![
                (3, vec![capture("first", 4), package("first", 0)]),
                (11, vec![capture("later", 9), package("later", 7)]),
                (29, Vec::new()),
            ],
            1,
            &packages,
        );
        let segment = ArtifactEntry::original(
            [2; 32],
            crate::certified_products::fixture_finalized_product(product, [2; 32]),
        )
        .unwrap();
        let values = ArtifactEntry::canonical(crate::certified_products::fixture_module_interface(
            [2; 32],
            "fixture",
            "Values",
            BTreeMap::new(),
        ));
        let outside = entry("Outside", &[]);
        let (segment_id, values_id, outside_id) = (
            segment.descriptor.id,
            values.descriptor.id,
            outside.descriptor.id,
        );
        let inventory = ArtifactInventory::default();
        let admitted = inventory
            .admit(&inventory.empty_view(), vec![segment, values, outside])
            .unwrap();
        let view = admitted.select_roots(vec![segment_id]).unwrap();
        let group = |original_ordinal| NativeRequirementRoot::Group {
            artifact: segment_id,
            original_ordinal,
        };
        let first = view.native_requirements_from_roots(&[group(3)]).unwrap();
        assert_eq!(
            first.bindings,
            vec![NativeBindingRequirement {
                artifact_id: values_id,
                identity: testing::identity("Values", "first"),
                generation: 4,
            }]
        );
        assert_eq!(
            first.packages,
            vec![NativePackageRequirement {
                artifact_id: segment_id,
                identity: package_identity("first"),
                generation: 0,
                interface_digest: package_digest,
            }]
        );
        assert_eq!(
            view.native_requirements_from_roots(&[group(29)]).unwrap(),
            NativeRequirements::default()
        );
        assert_eq!(
            view.native_requirements_from_roots(&[group(3), group(3)])
                .unwrap(),
            first
        );
        let all = view
            .native_requirements_from_roots(&[NativeRequirementRoot::AllGroups(segment_id)])
            .unwrap();
        assert_eq!(all.bindings.len(), 2);
        assert_eq!(all.packages.len(), 2);
        assert_eq!(
            all,
            view.native_requirements_from_roots(&[group(3), group(11), group(29)])
                .unwrap()
        );
        for (artifact, original_ordinal) in [(segment_id, 0), (segment_id, 30), (values_id, 3)] {
            assert!(
                matches!(view.native_requirements_from_roots(&[NativeRequirementRoot::Group { artifact, original_ordinal }]),
                Err(CompileError::ArtifactInventory(error)) if error.failure == ArtifactInventoryFailure::NativeGroupUnavailable { artifact, original_ordinal })
            );
        }
        for artifact in [outside_id, ArtifactId([255; 32])] {
            assert!(
                matches!(view.native_requirements_from_roots(&[NativeRequirementRoot::Group { artifact, original_ordinal: 3 }]),
                Err(CompileError::ArtifactInventory(error)) if error.failure == ArtifactInventoryFailure::NativeRootOutsideView { artifact })
            );
        }
        // Demand selection leaves the original module and its later groups in
        // custody; it only narrows which native obligations this call requests.
        assert!(view.artifact_ids().contains(&segment_id));
        drop(admitted);
        drop(view);
        assert_eq!(inventory.node_count(), 0);
    }

    #[test]
    fn indirect_native_group_requires_capture_absent_from_root_globals() {
        use crate::certified_products::PendingImportOwner;
        use tidepool_repr::execution_schema::testing;

        let capture_identity = testing::identity("Values", "first");
        let child_for_generation = |generation| {
            issued_native_groups(
                "Child",
                vec![(
                    7,
                    vec![PendingImportOwner::Retained {
                        identity: capture_identity.clone(),
                        generation,
                    }],
                )],
                &[],
                &BTreeMap::new(),
            )
        };
        let child = child_for_generation(4);
        let root = issued_native_groups(
            "Root",
            vec![(2, vec![issued_source(&child, 7)])],
            &[],
            &BTreeMap::new(),
        );
        let values = ArtifactEntry::canonical(crate::certified_products::fixture_module_interface(
            [2; 32],
            "fixture",
            "Values",
            BTreeMap::new(),
        ));
        let ArtifactPayload::Original(product) = &root.payload else {
            panic!("native root");
        };
        let globals = crate::certified_products::original_native_group_definitions(product, 2)
            .unwrap()
            .globals();
        assert_eq!(globals.len(), 1);
        assert_eq!(globals[0].identity.module, "Child");
        assert_eq!(globals[0].required_generation, None);
        assert!(root
            .native_requirements_for(2)
            .iter()
            .all(|(_, edge)| { matches!(edge, ArtifactDependency::NativeGroup { .. }) }));

        let inventory = ArtifactInventory::default();
        for offered_child in [None, Some(child_for_generation(5))] {
            let mut entries = vec![root.clone(), values.clone()];
            entries.extend(offered_child);
            assert!(matches!(
                inventory.admit(&inventory.empty_view(), entries),
                Err(CompileError::ArtifactInventory(error))
                    if matches!(error.failure, ArtifactInventoryFailure::MissingDependency { .. })
            ));
            assert_eq!(inventory.node_count(), 0);
        }
        let root_id = root.descriptor.id;
        let expected = NativeRequirements {
            bindings: vec![NativeBindingRequirement {
                artifact_id: values.descriptor.id,
                identity: capture_identity,
                generation: 4,
            }],
            packages: vec![],
        };
        let admitted = inventory
            .admit(&inventory.empty_view(), vec![root, child, values])
            .unwrap();
        let selected = admitted.select_roots(vec![root_id]).unwrap();
        let demand = NativeRequirementRoot::Group {
            artifact: root_id,
            original_ordinal: 2,
        };
        assert_eq!(
            selected.native_requirements_from_roots(&[demand]).unwrap(),
            expected
        );
        assert_eq!(
            selected
                .native_requirements_from_roots(&[demand, demand])
                .unwrap(),
            expected
        );
        assert!(matches!(
            selected.native_requirements_from_roots(&[NativeRequirementRoot::Group {
                artifact: root_id,
                original_ordinal: 7,
            }]),
            Err(CompileError::ArtifactInventory(error))
                if error.failure == ArtifactInventoryFailure::NativeGroupUnavailable {
                    artifact: root_id,
                    original_ordinal: 7,
                }
        ));
        drop(selected);
        drop(admitted);
        assert_eq!(inventory.node_count(), 0);
    }

    #[test]
    fn native_package_obligations_follow_selected_groups_without_package_artifact_nodes() {
        let inventory = ArtifactInventory::default();
        let package_file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(package_file.path(), b"native package interface").unwrap();
        let package_digest = digest(b"native package interface");
        let packages = BTreeMap::from([(
            ("ghc-internal".into(), "GHC.Internal.Base".into()),
            crate::certified_products::PackageInterfaceWitness {
                selected_path: package_file.path().to_path_buf(),
                sha256: package_digest,
            },
        )]);
        let package =
            |occurrence: &str| crate::certified_products::PendingImportOwner::RetainedPackage {
                unit: "ghc-internal".into(),
                module: "GHC.Internal.Base".into(),
                binder: tidepool_repr::execution_schema::SymbolIdentity {
                    unit: "ghc-internal".into(),
                    module: "GHC.Internal.Base".into(),
                    namespace: "value".into(),
                    occurrence: occurrence.into(),
                    record_parent: None,
                },
                generation: 0,
                interface_digest: package_digest,
            };
        let helper = issued_native_groups(
            "Helper",
            vec![(7, vec![package("map")]), (8, vec![package("foldr")])],
            &[],
            &packages,
        );
        let root = issued_native_groups(
            "Root",
            vec![(2, vec![issued_source(&helper, 7)])],
            &[],
            &BTreeMap::new(),
        );
        let root_id = root.descriptor.id;
        let helper_id = helper.descriptor.id;
        let view = inventory
            .admit(&inventory.empty_view(), vec![root, helper])
            .unwrap();
        let requirements = view
            .native_requirements_from_roots(&[NativeRequirementRoot::AllGroups(root_id)])
            .unwrap();
        assert!(requirements.bindings.is_empty());
        assert_eq!(requirements.packages.len(), 1);
        assert_eq!(requirements.packages[0].artifact_id, helper_id);
        assert_eq!(requirements.packages[0].identity.occurrence, "map");
        assert_eq!(
            view.native_requirements_from_roots(&[NativeRequirementRoot::AllGroups(helper_id)])
                .unwrap()
                .packages
                .len(),
            2
        );
        assert_eq!(inventory.metrics().nodes, 7);
    }
}
