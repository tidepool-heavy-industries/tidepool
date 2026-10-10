//! Checksummed declaration recovery graph. The manifest is metadata only;
//! executable bytes stay owned by the toolchain cache and its run-owned copy.

use crate::session::RecoveryPublicationWork;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use tidepool_repr::Generation;
use tidepool_toolchain::artifact_inventory::{ArtifactDependency, ArtifactDescriptor, ArtifactId};
use tidepool_toolchain::declaration_join::{ExactLexicalNode, ExactModuleIdentity};
use tidepool_toolchain::recovery_artifacts::{
    capture_recovery_artifact_observations, RecoveryArtifactError, RecoveryArtifactRef,
    RecoveryArtifactWork, RecoveryJoinRef, RecoveryModuleInterfaceRef, RecoveryValueInterfaceRef,
    RetainedRecoveryArtifactObservations,
};

#[path = "newrecovery_v2/snapshots.rs"]
mod snapshots;
use snapshots::{GraphEncoding, GraphRead};
pub(crate) use snapshots::{RecoveryGraph, RecoveryGraphCandidate};

const VERSION: u32 = 9;
const PAIRED_PUBLIC_SCHEMA: &str = "paired-public-v9";
const MAX_MANIFEST_BYTES: usize = 64 << 20;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RecoveryGraphWire {
    pub version: u32,
    pub public_schema: String,
    pub source_session: u64,
    pub lineage: u64,
    #[serde(with = "generation_serde")]
    pub high_water: Generation,
    /// Several inherited-context actors can publish distinct lexical views
    /// from the same session's declaration/artifact DAG.
    pub public_surfaces: Vec<RecoveryPublicSurface>,
    pub nodes: Vec<RecoveryNode>,
    pub artifacts: Vec<RecoveryArtifactClosure>,
    /// Direct compiler inventory requirements; these describe dependency facts
    /// but do not grant native binding leases.
    pub artifact_dependencies: Vec<RecoveryArtifactDependency>,
    pub checksum: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RecoveryArtifactDependency {
    pub source: ArtifactId,
    pub target: ArtifactId,
    pub dependency: ArtifactDependency,
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryPublicOwner {
    path: String,
    incarnation: u64,
}

impl RecoveryPublicOwner {
    pub fn new(path: &tidepool_repr::ActorPath, incarnation: u64) -> Option<Self> {
        if incarnation == 0 {
            return None;
        }
        Some(Self {
            path: path.to_string(),
            incarnation,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RecoveryPublicSurface {
    pub owner: RecoveryPublicOwner,
    #[serde(with = "option_generation_serde")]
    pub declaration_root: Option<Generation>,
    pub epoch: u64,
    /// Final visible binding winners. Their values are process-local, so
    /// recovery projects these as tombstones without reviving an older name.
    pub bindings: Vec<RecoveryPublicBinding>,
    /// Exact machine-local source instances at this public tip. Recovery can
    /// report their loss but cannot reconstruct their mutable CAF state.
    pub source_instances: Vec<RecoveryPublicSourceInstance>,
    /// Exact published compiler selection, including a source-only Generation0
    /// surface. Immutable artifact authentication precedes role reissuance.
    pub compiler_context: RecoveryCompilerContext,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RecoveryCompilerContext {
    pub artifact_refs: Vec<ArtifactId>,
    pub native_groups: Vec<tidepool_toolchain::artifact_inventory::NativeGroupKey>,
    pub compiler_roles: Vec<tidepool_toolchain::artifact_inventory::CompilerInputRole>,
    pub lexical: Vec<ExactLexicalNode>,
}

impl RecoveryCompilerContext {
    pub(crate) fn capture(
        context: &tidepool_toolchain::declaration_join::ExactDeclarationContext,
    ) -> Self {
        let mut captured = Self {
            artifact_refs: context
                .artifact_view()
                .descriptors()
                .into_iter()
                .map(|row| row.id)
                .collect(),
            native_groups: context
                .artifact_view()
                .selected_native_groups()
                .into_iter()
                .collect(),
            compiler_roles: context.compiler_input_roles(),
            lexical: context.lexical_graph().to_vec(),
        };
        captured.normalize();
        captured
    }

    /// Lexical nodes and import edges describe an unordered exact graph.
    /// Capture and durable sealing use the same representation for equality;
    /// repeated lexical facts remain invalid rather than being deduplicated.
    fn normalize(&mut self) {
        self.artifact_refs.sort();
        self.artifact_refs.dedup();
        self.native_groups.sort();
        self.lexical.sort_by(|a, b| a.owner.cmp(&b.owner));
        for lexical in &mut self.lexical {
            lexical.imports.sort();
        }
    }

    pub(crate) fn restore(
        &self,
        inventory: &tidepool_toolchain::declaration_join::RecoveredArtifactInventory,
    ) -> Result<tidepool_toolchain::declaration_join::ExactDeclarationContext, crate::CompileError>
    {
        inventory.context_with_published_roles(
            &self.artifact_refs,
            &self.native_groups,
            &self.compiler_roles,
            self.lexical.clone(),
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RecoveryPublicBinding {
    pub name: String,
    pub owner: RecoveryBindingId,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RecoveryLostPublicBinding {
    pub name: String,
    pub winner: RecoveryBindingId,
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RecoverySourceIdentity {
    pub unit: String,
    pub module: String,
    pub namespace: String,
    pub occurrence: String,
    pub record_parent: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RecoveryPublicSourceInstance {
    pub machine_incarnation: u64,
    pub instance: u32,
    pub module_version: [u8; 32],
    pub binder: RecoverySourceIdentity,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RecoveryNode {
    #[serde(with = "generation_serde")]
    pub id: Generation,
    /// Lexical visibility ancestry. A Join's implementation dependencies are
    /// separate and never become its public declaration parent.
    #[serde(with = "option_generation_serde")]
    pub parent: Option<Generation>,
    pub kind: RecoveryNodeKind,
    #[serde(with = "generation_vec_serde")]
    pub implementation_refs: Vec<Generation>,
    /// Exact source modules visible at this declaration tip. These identities
    /// and edges describe lexical authority independently of implementation
    /// artifact reachability.
    pub lexical_roots: Vec<ExactModuleIdentity>,
    pub lexical: Vec<ExactLexicalNode>,
    pub artifact_refs: Vec<ArtifactId>,
    /// Exact admitted native-group closure, independently of full-product
    /// custody. Required even when empty; older records cannot infer it.
    pub native_groups: Vec<tidepool_toolchain::artifact_inventory::NativeGroupKey>,
    /// Authenticated compiler namespace roles, independent of native custody.
    /// This field is required even when no original is offered for reuse.
    pub compiler_roles: Vec<tidepool_toolchain::artifact_inventory::CompilerInputRole>,
    pub exports: Vec<RecoveryExport>,
    pub retracts: Vec<RecoverySymbolIdentity>,
    /// GHC-normalized import specifications introduced by this turn. These
    /// are metadata for rebuilding the next workbench context, never replayed
    /// as declaration source.
    pub workbench_imports: Vec<String>,
    pub instances: RecoveryInstanceInventory,
    pub live_dependencies: Vec<RecoveryLiveDependency>,
    pub state: RecoveryNodeState,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RecoveryNodeKind {
    Authored,
    Join,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub(crate) enum RecoveryNodeState {
    ExactArtifactClosure,
    LiveValueDependency { reason: String },
    MissingArtifactClosure { reason: String },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "reference", rename_all = "snake_case")]
pub(crate) enum RecoveryArtifactClosure {
    Home(RecoveryArtifactRef),
    Join(RecoveryJoinRef),
    ModuleInterface(RecoveryModuleInterfaceRef),
    ValueInterface(RecoveryValueInterfaceRef),
}

impl RecoveryArtifactClosure {
    pub(crate) fn descriptor(&self) -> ArtifactDescriptor {
        match self {
            Self::Home(reference) => ArtifactDescriptor::from_recovery_product(reference),
            Self::Join(reference) => ArtifactDescriptor::from_recovery_join(reference),
            Self::ModuleInterface(reference) => {
                ArtifactDescriptor::from_recovery_module_interface(reference)
            }
            Self::ValueInterface(reference) => {
                ArtifactDescriptor::from_recovery_value_interface(reference)
            }
        }
    }

    #[must_use]
    pub(crate) fn artifact_id(&self) -> ArtifactId {
        self.descriptor().id
    }

    fn declared_artifact_id(&self) -> ArtifactId {
        match self {
            Self::ValueInterface(reference) => reference.artifact_id,
            _ => self.artifact_id(),
        }
    }

    fn paths(&self) -> (&Path, Option<&Path>) {
        match self {
            Self::Home(reference) => (&reference.interface_path, Some(&reference.product_path)),
            Self::Join(reference) => (&reference.interface_path, None),
            Self::ModuleInterface(reference) => (&reference.interface.interface_path, None),
            Self::ValueInterface(reference) => (&reference.interface.interface_path, None),
        }
    }

    /// Every durable payload is checked before the toolchain reads it, including
    /// the canonical interface companion nested in an original native product.
    fn component_paths(&self) -> Vec<(RecoveryArtifactComponent, &Path)> {
        use RecoveryArtifactComponent as C;
        fn interface_paths<'a>(reference: &'a RecoveryJoinRef, paths: &mut Vec<(C, &'a Path)>) {
            paths.push((C::Interface, &reference.interface_path));
            paths.push((C::PackageImports, &reference.package_imports_path));
        }
        fn module_paths<'a>(
            reference: &'a RecoveryModuleInterfaceRef,
            paths: &mut Vec<(C, &'a Path)>,
        ) {
            interface_paths(&reference.interface, paths);
            paths.push((C::Certificate, &reference.certificate_path));
            if let Some(core) = &reference.core {
                paths.push((C::Core, &core.path));
            }
        }
        let mut paths = Vec::new();
        match self {
            Self::Home(reference) => {
                paths.push((C::Interface, reference.interface_path.as_path()));
                paths.push((C::Product, reference.product_path.as_path()));
                paths.push((C::PackageImports, reference.package_imports_path.as_path()));
                paths.push((C::Certificate, reference.certification_path.as_path()));
                if let Some(source) = &reference.execution_source {
                    paths.push((C::ExecutionSource, source.path.as_path()));
                }
                if let Some(interface) = &reference.module_interface {
                    module_paths(interface, &mut paths);
                }
            }
            Self::Join(reference) => interface_paths(reference, &mut paths),
            Self::ModuleInterface(reference) => module_paths(reference, &mut paths),
            Self::ValueInterface(reference) => interface_paths(&reference.interface, &mut paths),
        }
        paths
    }

    fn owner(&self) -> ExactModuleIdentity {
        self.descriptor().owner
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RecoverySymbolIdentity {
    pub unit: String,
    pub module: String,
    /// GHC namespace tag, preserved as a value rather than interpreted from
    /// a rendered name. The compiler adapter owns mapping to GHC constructors.
    pub namespace: String,
    pub occurrence: String,
    pub record_parent: Option<Box<RecoverySymbolIdentity>>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RecoveryExport {
    pub identity: RecoverySymbolIdentity,
    pub kind: RecoveryExportKind,
    /// Exact GHC identities of constructors, selectors, methods, or associated
    /// type members visible through this head.
    pub children: Vec<RecoverySymbolIdentity>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RecoveryExportKind {
    Value,
    Type,
    Class,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RecoveryInstanceEvidence {
    pub dfun: RecoverySymbolIdentity,
    pub class: RecoverySymbolIdentity,
    pub selected: bool,
    pub selected_axioms: Vec<RecoverySymbolIdentity>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RecoveryInstanceInventory {
    pub classes: Vec<RecoveryInstanceEvidence>,
    pub selected_family_axioms: Vec<RecoverySymbolIdentity>,
    /// Full consistency closure is retained independently of the selected
    /// reduction surface, so a hidden incompatible family axiom stays known.
    pub family_consistency_closure: Vec<RecoverySymbolIdentity>,
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(tag = "dependency", rename_all = "snake_case")]
pub(crate) enum RecoveryLiveDependency {
    NativeBinding {
        artifact_id: ArtifactId,
        binding: RecoverySourceIdentity,
        generation: u64,
    },
    Instance {
        dfun: RecoverySymbolIdentity,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RecoveryBindingId {
    pub session: u64,
    pub variable: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RecoveryTombstone {
    pub identity: RecoverySymbolIdentity,
    pub winner: Generation,
    pub reason: RecoveryLossReason,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum RecoveryLossReason {
    LiveValueDependency(String),
    MissingArtifactClosure(String),
    Artifact(RecoveryArtifactLoss),
    Dependency {
        generation: Generation,
        detail: String,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RecoveryArtifactComponent {
    Interface,
    Product,
    PackageImports,
    Certificate,
    Core,
    ExecutionSource,
    ExternalPackageInterface,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum RecoveryArtifactLossKind {
    Missing,
    Unreadable(String),
    DigestMismatch,
    ExecutionSourceProducerMismatch {
        unit: String,
        module: String,
        expected: [u8; 32],
        actual: [u8; 32],
    },
    UnsupportedPackageImportsVersion {
        found: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RecoveryArtifactLoss {
    pub component: RecoveryArtifactComponent,
    pub path: PathBuf,
    pub kind: RecoveryArtifactLossKind,
}

pub(crate) struct RecoveryV2Read {
    pub graph: RecoveryGraph,
    pub artifact_losses: BTreeMap<ArtifactId, Vec<RecoveryArtifactLoss>>,
    pub inventory: Option<tidepool_toolchain::declaration_join::RecoveredArtifactInventory>,
    observations: Option<RetainedRecoveryArtifactObservations>,
}

/// Complete sealed public graph facts and writer/reader-issued physical
/// observations. Live admission rechecks files without rebuilding those facts.
#[derive(Clone)]
pub(crate) struct RetainedRecoveryGraph {
    graph: RecoveryGraph,
    manifest_digest: blake3::Hash,
    observations: RetainedRecoveryArtifactObservations,
}

impl RetainedRecoveryGraph {
    pub(crate) fn observe(
        &self,
        graph: &RecoveryGraph,
        path: &Path,
        root: &Path,
    ) -> Result<bool, RecoveryError> {
        if self.graph.checksum() != graph.checksum() {
            return Ok(false);
        }
        let file = fs::File::open(path).map_err(|error| at(path, error.to_string()))?;
        let mut bytes = Vec::new();
        file.take(MAX_MANIFEST_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| at(path, error.to_string()))?;
        if bytes.len() > MAX_MANIFEST_BYTES {
            return Err(at(path, "recovery manifest exceeds the bounded size"));
        }
        if blake3::hash(&bytes) != self.manifest_digest {
            return Ok(false);
        }
        self.observations
            .observe(root, &mut RecoveryArtifactWork::default())
            .map_err(|error| at(path, error.to_string()))?;
        Ok(true)
    }
}

#[derive(Clone, Copy)]
pub(crate) enum RecoveryReadPurpose {
    Metadata,
    Hydration,
}

impl RecoveryV2Read {
    pub(crate) fn retain(&self, bytes: &[u8]) -> Option<RetainedRecoveryGraph> {
        if !self.artifact_losses.is_empty() {
            return None;
        }
        Some(RetainedRecoveryGraph {
            graph: self.graph.clone(),
            manifest_digest: blake3::hash(bytes),
            observations: self.observations.clone()?,
        })
    }
    pub(crate) fn projection(
        &self,
        owner: &RecoveryPublicOwner,
    ) -> Result<BTreeMap<RecoverySymbolIdentity, RecoveryHead>, RecoveryError> {
        self.graph.projection(owner, &self.artifact_losses)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum RecoveryHead {
    Available {
        winner: Generation,
        export: RecoveryExport,
    },
    Tombstone(RecoveryTombstone),
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum RecoveryAvailability {
    Interface,
    Native,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RecoveryError {
    pub path: Option<PathBuf>,
    pub detail: String,
    pub kind: RecoveryErrorKind,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum RecoveryErrorKind {
    SourceEvidence {
        input: PathBuf,
        failure: tidepool_toolchain::cache::DependencyEvidenceFailure,
    },
    Manifest,
    Format(RecoveryRefusal),
    InventoryAccounting(tidepool_toolchain::recovery_artifacts::RecoveryAdmissionFailure),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecoveryRefusal {
    UnsupportedOldFormat { version: u64 },
    UnsupportedFutureFormat { version: u64 },
}

pub(crate) struct StagedRecoveryManifest {
    inner: tidepool_atomic_write::StagedDurableWrite,
    graph: RecoveryGraph,
    pub(crate) work: RecoveryPublicationWork,
    manifest_digest: blake3::Hash,
    observations: Option<RetainedRecoveryArtifactObservations>,
}

/// A graph whose shape and checksum have been checked, or whose checksum was
/// just produced by `seal`. Keeping that fact in the type lets staging avoid
/// repeating the same full-graph validation at each private layer.
struct ValidatedRecoveryGraph(
    RecoveryGraph,
    RecoveryPublicationWork,
    Option<RetainedRecoveryArtifactObservations>,
);

impl ValidatedRecoveryGraph {
    fn check(graph: RecoveryGraph) -> Result<Self, RecoveryError> {
        let (graph, encoded_bytes) = graph.into_staging_snapshot_with_encoded_bytes()?;
        Ok(Self(
            graph,
            RecoveryPublicationWork {
                checksum_encode_bytes: encoded_bytes,
                ..Default::default()
            },
            None,
        ))
    }

    fn seal(graph: RecoveryGraphCandidate) -> Result<Self, RecoveryError> {
        let (graph, encoded_bytes) = graph.seal_with_encoded_bytes()?;
        Ok(Self(
            graph,
            RecoveryPublicationWork {
                checksum_encode_bytes: encoded_bytes,
                ..Default::default()
            },
            None,
        ))
    }

    fn validate_artifact_files(
        &mut self,
        root: &Path,
    ) -> Result<BTreeMap<ArtifactId, Vec<RecoveryArtifactLoss>>, RecoveryError> {
        let mut work = RecoveryArtifactWork::default();
        let result =
            self.0
                .capture_artifact_observations(root, &mut work)
                .map(|(losses, observations)| {
                    if losses.is_empty() {
                        self.2 = Some(observations);
                    }
                    losses
                });
        self.1.recovery_validation_hash_bytes += work.hash_bytes;
        result
    }
}

pub(crate) enum RecoveryPublishOutcome {
    Durable {
        graph: RecoveryGraph,
        publication: tidepool_atomic_write::PublishedWrite,
    },
    BeforeRename {
        path: PathBuf,
        detail: String,
    },
    PublishedDurabilityUnconfirmed {
        graph: RecoveryGraph,
        publication: tidepool_atomic_write::PublishedWrite,
        detail: String,
    },
}

impl StagedRecoveryManifest {
    pub(crate) fn retained_graph(
        &self,
        previous: Option<&RetainedRecoveryGraph>,
    ) -> Option<RetainedRecoveryGraph> {
        let observations = self.observations.clone().or_else(|| {
            previous
                .filter(|previous| previous.graph.artifacts().eq(self.graph.artifacts()))
                .map(|previous| previous.observations.clone())
        })?;
        Some(RetainedRecoveryGraph {
            graph: self.graph.clone(),
            manifest_digest: self.manifest_digest,
            observations,
        })
    }
    #[must_use]
    pub(crate) fn candidate_graph(&self) -> &RecoveryGraph {
        &self.graph
    }

    /// Publication preserves the distinction required by recovery: only
    /// `Durable` permits an ordinary successful state transition.
    pub(crate) fn publish(self) -> RecoveryPublishOutcome {
        match self.inner.publish() {
            Ok(publication) => RecoveryPublishOutcome::Durable {
                graph: self.graph,
                publication,
            },
            Err(tidepool_atomic_write::PublishError::BeforeRename(error)) => {
                RecoveryPublishOutcome::BeforeRename {
                    path: error.path,
                    detail: error.source.to_string(),
                }
            }
            Err(tidepool_atomic_write::PublishError::PublishedDurabilityUnconfirmed {
                publication,
                source,
            }) => RecoveryPublishOutcome::PublishedDurabilityUnconfirmed {
                graph: self.graph,
                publication,
                detail: source.to_string(),
            },
        }
    }
}

pub(crate) fn stage_v2(
    path: &Path,
    recovery_root: &Path,
    graph: RecoveryGraph,
) -> Result<StagedRecoveryManifest, RecoveryError> {
    stage_validated_v2(path, recovery_root, ValidatedRecoveryGraph::check(graph)?)
}

fn stage_validated_v2(
    path: &Path,
    recovery_root: &Path,
    mut graph: ValidatedRecoveryGraph,
) -> Result<StagedRecoveryManifest, RecoveryError> {
    if !graph.validate_artifact_files(recovery_root)?.is_empty() {
        return Err(error(
            "cannot stage a recovery graph with unavailable or corrupt artifacts",
        ));
    }
    stage_metadata_v2(path, graph)
}

fn stage_metadata_v2(
    path: &Path,
    graph: ValidatedRecoveryGraph,
) -> Result<StagedRecoveryManifest, RecoveryError> {
    let ValidatedRecoveryGraph(graph, mut work, observations) = graph;
    let bytes = serde_json::to_vec(&graph)
        .map_err(|e| error(format!("could not encode recovery graph: {e}")))?;
    if bytes.len() > MAX_MANIFEST_BYTES {
        return Err(error("recovery manifest exceeds the bounded size"));
    }
    let staged = tidepool_atomic_write::stage_durable(path, &bytes).map_err(|e| {
        at(
            &e.path,
            format!("could not stage recovery manifest: {}", e.source),
        )
    })?;
    work.manifest_write_bytes += bytes.len() as u64;
    Ok(StagedRecoveryManifest {
        inner: staged,
        graph,
        work,
        manifest_digest: blake3::hash(&bytes),
        observations,
    })
}

/// Reserve the next burned identity durably before exposing its generated
/// module name to compilation. No node or public surface moves.
pub(crate) fn stage_high_water_v2(
    path: &Path,
    graph: &RecoveryGraph,
    next: Generation,
) -> Result<StagedRecoveryManifest, RecoveryError> {
    let candidate = high_water_candidate(graph, next)?;
    stage_metadata_v2(path, candidate)
}

/// Reserve one contiguous identity range through the metadata writer.
pub(crate) fn stage_high_water_range_v2(
    path: &Path,
    graph: &RecoveryGraph,
    count: u64,
) -> Result<StagedRecoveryManifest, RecoveryError> {
    graph.validate()?;
    if count == 0 {
        return Err(error("recovery reservation range must be nonempty"));
    }
    let next = Generation(
        graph
            .high_water
            .0
            .checked_add(count)
            .ok_or_else(|| error("recovery generation space exhausted"))?,
    );
    let mut candidate = graph.candidate();
    candidate.set_high_water(next)?;
    stage_metadata_v2(path, ValidatedRecoveryGraph::seal(candidate)?)
}

/// Stage a binding/source-only public visibility change without allocating a
/// synthetic declaration node. The caller supplies the already checked final
/// winners; the manifest, rather than a process-local machine id, is the
/// authority for whether that public change occurred. A per-owner epoch check
/// here does not compare the whole manifest with the file at rename time. The
/// owning commit path must revalidate the exact graph checksum after staging
/// and serialize read, stage, and rename under one session checkout; otherwise
/// two actors can publish copies that erase one another's independent surface.
pub(crate) fn stage_public_visibility_v2(
    path: &Path,
    recovery_root: &Path,
    graph: &RecoveryGraph,
    owner: RecoveryPublicOwner,
    expected_epoch: u64,
    bindings: Vec<RecoveryPublicBinding>,
    source_instances: Vec<RecoveryPublicSourceInstance>,
    initial_declaration_root: Option<Generation>,
    compiler_context: RecoveryCompilerContext,
) -> Result<StagedRecoveryManifest, RecoveryError> {
    let next_epoch = expected_epoch
        .checked_add(1)
        .ok_or_else(|| error("public visibility epoch exhausted"))?;
    stage_public_visibility_at_epoch_v2(
        path,
        recovery_root,
        graph,
        owner,
        expected_epoch,
        next_epoch,
        bindings,
        source_instances,
        initial_declaration_root,
        compiler_context,
    )
}

pub(crate) fn stage_public_visibility_at_epoch_v2(
    path: &Path,
    recovery_root: &Path,
    graph: &RecoveryGraph,
    owner: RecoveryPublicOwner,
    expected_epoch: u64,
    next_epoch: u64,
    bindings: Vec<RecoveryPublicBinding>,
    source_instances: Vec<RecoveryPublicSourceInstance>,
    initial_declaration_root: Option<Generation>,
    compiler_context: RecoveryCompilerContext,
) -> Result<StagedRecoveryManifest, RecoveryError> {
    if next_epoch <= expected_epoch {
        return Err(error("public visibility epoch must advance"));
    }
    graph.validate()?;
    let mut candidate = graph.candidate();
    let mut surface = match graph.surface(&owner) {
        Some(surface) => RecoveryPublicSurface {
            owner,
            declaration_root: surface.declaration_root,
            epoch: surface.epoch,
            bindings,
            source_instances,
            compiler_context,
        },
        None if expected_epoch == 0 => RecoveryPublicSurface {
            owner,
            declaration_root: initial_declaration_root,
            epoch: 0,
            bindings,
            source_instances,
            compiler_context,
        },
        None => return Err(error("paired public surface is missing")),
    };
    if surface.epoch != expected_epoch {
        return Err(error("paired public visibility epoch changed"));
    }
    surface.epoch = next_epoch;
    candidate.replace_surface(surface);
    let candidate = ValidatedRecoveryGraph::seal(candidate)?;
    stage_validated_v2(path, recovery_root, candidate)
}

fn high_water_candidate(
    graph: &RecoveryGraph,
    next: Generation,
) -> Result<ValidatedRecoveryGraph, RecoveryError> {
    graph.validate()?;
    let expected = graph
        .high_water
        .0
        .checked_add(1)
        .map(Generation)
        .ok_or_else(|| error("recovery generation space exhausted"))?;
    if next != expected {
        return Err(error(format!(
            "recovery reservation must be generation {}, got {}",
            expected.0, next.0
        )));
    }
    let mut candidate = graph.candidate();
    candidate.set_high_water(next)?;
    ValidatedRecoveryGraph::seal(candidate)
}

/// Read the current graph. Unsupported formats are refused without rewriting them.
pub(crate) fn read_v2(
    path: &Path,
    recovery_root: &Path,
) -> Result<Option<RecoveryV2Read>, RecoveryError> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(at(
                path,
                format!("could not read recovery manifest: {error}"),
            ));
        }
    };
    read_v2_bytes(path, recovery_root, &bytes, RecoveryReadPurpose::Metadata)
}

/// Validate bytes read once by the configured manifest owner.
pub(crate) fn read_v2_bytes(
    path: &Path,
    recovery_root: &Path,
    bytes: &[u8],
    purpose: RecoveryReadPurpose,
) -> Result<Option<RecoveryV2Read>, RecoveryError> {
    if bytes.len() > MAX_MANIFEST_BYTES {
        return Err(at(path, "recovery manifest exceeds the bounded size"));
    }
    let value: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|e| at(path, format!("invalid recovery manifest JSON: {e}")))?;
    let version = value
        .get("version")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| at(path, "recovery manifest has no integer version"))?;
    if version < u64::from(VERSION) {
        return Err(refusal_at(
            path,
            RecoveryRefusal::UnsupportedOldFormat { version },
        ));
    }
    if version > u64::from(VERSION) {
        return Err(refusal_at(
            path,
            RecoveryRefusal::UnsupportedFutureFormat { version },
        ));
    }
    if value
        .get("public_schema")
        .and_then(serde_json::Value::as_str)
        != Some(PAIRED_PUBLIC_SCHEMA)
    {
        return Err(at(
            path,
            "v9 recovery graph lacks the supported paired-public-v9 schema",
        ));
    }
    let graph: RecoveryGraph = serde_json::from_value(value)
        .map_err(|e| at(path, format!("invalid v9 recovery graph: {e}")))?;
    graph.validate().map_err(|mut error| {
        error.path = Some(path.to_path_buf());
        error
    })?;
    let mut observations = None;
    let (inventory, artifact_losses) = match purpose {
        RecoveryReadPurpose::Metadata => {
            let (losses, observed) = graph
                .capture_artifact_observations(recovery_root, &mut RecoveryArtifactWork::default())
                .map_err(|mut error| {
                    error.path = Some(path.to_path_buf());
                    error
                })?;
            observations = Some(observed);
            (None, losses)
        }
        RecoveryReadPurpose::Hydration => match graph.capture_inventory(recovery_root) {
            Ok(inventory) => (Some(inventory), BTreeMap::new()),
            Err(tidepool_toolchain::declaration_join::RecoveryInventoryError::Artifacts(
                errors,
            )) => {
                let artifacts = graph
                    .artifacts()
                    .map(|artifact| (artifact.artifact_id(), artifact))
                    .collect::<BTreeMap<_, _>>();
                let losses = errors
                    .into_iter()
                    .map(|(id, error)| Ok((id, vec![artifact_error_loss(artifacts[&id], error)?])))
                    .collect::<Result<_, RecoveryError>>()
                    .map_err(|mut error| {
                        error.path = Some(path.to_path_buf());
                        error
                    })?;
                (None, losses)
            }
            Err(error) => return Err(at(path, error.to_string())),
        },
    };
    Ok(Some(RecoveryV2Read {
        graph,
        artifact_losses,
        inventory,
        observations,
    }))
}

impl std::fmt::Display for RecoveryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(path) = &self.path {
            write!(f, "{}: {}", path.display(), self.detail)
        } else {
            f.write_str(&self.detail)
        }
    }
}

impl std::error::Error for RecoveryError {}

impl RecoveryGraph {
    pub(crate) fn capture_inventory(
        &self,
        root: &Path,
    ) -> Result<
        tidepool_toolchain::declaration_join::RecoveredArtifactInventory,
        tidepool_toolchain::declaration_join::RecoveryInventoryError,
    > {
        let mut products = Vec::new();
        let mut module_interfaces = Vec::new();
        let mut joins = Vec::new();
        let mut values = Vec::new();
        for artifact in self.artifacts() {
            match artifact {
                RecoveryArtifactClosure::Home(reference) => products.push(reference.clone()),
                RecoveryArtifactClosure::Join(reference) => joins.push(reference.clone()),
                RecoveryArtifactClosure::ModuleInterface(reference) => {
                    module_interfaces.push(reference.clone())
                }
                RecoveryArtifactClosure::ValueInterface(reference) => {
                    values.push(reference.clone())
                }
            }
        }
        let descriptors = self
            .artifacts()
            .map(|artifact| artifact.descriptor())
            .collect::<Vec<_>>();
        let interfaces = self
            .artifact_dependencies()
            .map(|edge| (edge.source, edge.target, edge.dependency.clone()))
            .collect::<Vec<_>>();
        tidepool_toolchain::declaration_join::RecoveredArtifactInventory::capture(
            root,
            &products,
            &module_interfaces,
            &joins,
            &values,
            &descriptors,
            &interfaces,
        )
    }

    pub(crate) fn empty(source_session: u64, lineage: u64) -> Result<Self, RecoveryError> {
        Self::from_wire(RecoveryGraphWire::empty(source_session, lineage)?)
    }

    /// An opaque snapshot was authenticated on decode or sealed by its owner.
    /// Its revision token may reflect a valid input's original array order.
    pub(crate) fn validate(&self) -> Result<(), RecoveryError> {
        Ok(())
    }

    /// Heap values are never recovered. Compute loss from the final winning
    /// names, so an unavailable replacement cannot reveal an older binding.
    pub(crate) fn public_binding_tombstones(
        &self,
        owner: &RecoveryPublicOwner,
    ) -> Result<Vec<RecoveryLostPublicBinding>, RecoveryError> {
        self.validate()?;
        let surface = self
            .public_surfaces()
            .find(|surface| &surface.owner == owner);
        Ok(surface
            .into_iter()
            .flat_map(|surface| &surface.bindings)
            .map(|binding| RecoveryLostPublicBinding {
                name: binding.name.clone(),
                winner: binding.owner,
            })
            .collect())
    }

    /// Return the visible heads at the exact public root. Lost winning nodes
    /// remain tombstones, suppressing older declarations with the same exact
    /// GHC identity instead of silently resurrecting them.
    pub(crate) fn projection(
        &self,
        owner: &RecoveryPublicOwner,
        artifact_losses: &BTreeMap<ArtifactId, Vec<RecoveryArtifactLoss>>,
    ) -> Result<BTreeMap<RecoverySymbolIdentity, RecoveryHead>, RecoveryError> {
        self.project_heads(owner, artifact_losses, RecoveryAvailability::Native)
    }

    /// Recover type and lexical evidence without granting native leases.
    /// Original products retain their exact live import requirements, which
    /// runtime admission checks only for the native groups actually demanded.
    pub(crate) fn interface_projection(
        &self,
        owner: &RecoveryPublicOwner,
        artifact_losses: &BTreeMap<ArtifactId, Vec<RecoveryArtifactLoss>>,
    ) -> Result<BTreeMap<RecoverySymbolIdentity, RecoveryHead>, RecoveryError> {
        self.project_heads(owner, artifact_losses, RecoveryAvailability::Interface)
    }

    fn project_heads(
        &self,
        owner: &RecoveryPublicOwner,
        artifact_losses: &BTreeMap<ArtifactId, Vec<RecoveryArtifactLoss>>,
        availability: RecoveryAvailability,
    ) -> Result<BTreeMap<RecoverySymbolIdentity, RecoveryHead>, RecoveryError> {
        self.validate()?;
        let Some(root) = self
            .public_surfaces()
            .find(|surface| &surface.owner == owner)
            .and_then(|surface| surface.declaration_root)
        else {
            return Ok(BTreeMap::new());
        };
        let by_id: BTreeMap<_, _> = self.nodes().map(|node| (node.id, node)).collect();
        let mut chain = Vec::new();
        let mut cursor = Some(root);
        while let Some(id) = cursor {
            let node = by_id
                .get(&id)
                .ok_or_else(|| error("public root ancestry is incomplete"))?;
            chain.push(*node);
            cursor = node.parent;
        }
        chain.reverse();
        let mut visible = BTreeMap::new();
        let mut memo = BTreeMap::new();
        for node in chain {
            let recoverability =
                node_recoverability(node.id, &by_id, artifact_losses, availability, &mut memo);
            for identity in &node.retracts {
                visible.remove(identity);
            }
            for export in &node.exports {
                if let Err(reason) = &recoverability {
                    visible.insert(
                        export.identity.clone(),
                        RecoveryHead::Tombstone(RecoveryTombstone {
                            identity: export.identity.clone(),
                            winner: node.id,
                            reason: reason.clone(),
                        }),
                    );
                } else {
                    visible.insert(
                        export.identity.clone(),
                        RecoveryHead::Available {
                            winner: node.id,
                            export: export.clone(),
                        },
                    );
                }
            }
        }
        Ok(visible)
    }

    /// Imports visible after the exact lexical chain ending at `root`.
    /// Preserve declaration order while removing specifications reintroduced
    /// verbatim by later turns, matching `SourceImports::extend`.
    pub(crate) fn workbench_imports(&self, root: Generation) -> Result<Vec<String>, RecoveryError> {
        self.validate()?;
        let by_id: BTreeMap<_, _> = self.nodes().map(|node| (node.id, node)).collect();
        let mut chain = Vec::new();
        let mut cursor = Some(root);
        while let Some(id) = cursor {
            let node = by_id
                .get(&id)
                .ok_or_else(|| error("workbench import ancestry is incomplete"))?;
            chain.push(*node);
            cursor = node.parent;
        }
        chain.reverse();
        let mut imports = Vec::new();
        for node in chain {
            for spec in &node.workbench_imports {
                if !imports.contains(spec) {
                    imports.push(spec.clone());
                }
            }
        }
        Ok(imports)
    }

    /// Validate every materialized artifact against the run-owned recovery
    /// root. Symlinks that escape the root are rejected.
    pub(crate) fn validate_artifact_files(
        &self,
        root: &Path,
    ) -> Result<BTreeMap<ArtifactId, Vec<RecoveryArtifactLoss>>, RecoveryError> {
        self.validate()?;
        self.validate_artifact_files_after_graph_validation(root)
    }

    fn validate_artifact_files_after_graph_validation(
        &self,
        root: &Path,
    ) -> Result<BTreeMap<ArtifactId, Vec<RecoveryArtifactLoss>>, RecoveryError> {
        self.validate_artifact_files_with_work(root, &mut RecoveryArtifactWork::default())
    }

    fn validate_artifact_files_with_work(
        &self,
        root: &Path,
        work: &mut RecoveryArtifactWork,
    ) -> Result<BTreeMap<ArtifactId, Vec<RecoveryArtifactLoss>>, RecoveryError> {
        self.capture_artifact_observations(root, work)
            .map(|(losses, _)| losses)
    }

    pub(crate) fn capture_artifact_observations(
        &self,
        root: &Path,
        work: &mut RecoveryArtifactWork,
    ) -> Result<
        (
            BTreeMap<ArtifactId, Vec<RecoveryArtifactLoss>>,
            RetainedRecoveryArtifactObservations,
        ),
        RecoveryError,
    > {
        let root = fs::canonicalize(root)
            .map_err(|e| error(format!("could not resolve recovery root: {e}")))?;
        capture_recovery_artifact_observations(&root, work, |verification| {
            let mut losses = BTreeMap::new();
            for artifact in self.artifacts() {
                for (_, path) in artifact.component_paths() {
                    validate_relative(path)?;
                }
                let key = artifact.artifact_id();
                let mut item_losses = Vec::new();
                let verification = match artifact {
                    RecoveryArtifactClosure::Home(reference) => {
                        verification.verify_home(reference).map(|_| ())
                    }
                    RecoveryArtifactClosure::Join(reference) => {
                        verification.verify_join(reference).map(|_| ())
                    }
                    RecoveryArtifactClosure::ModuleInterface(reference) => {
                        verification.verify_module_interface(reference)
                    }
                    RecoveryArtifactClosure::ValueInterface(reference) => {
                        verification.verify_join(&reference.interface).map(|_| ())
                    }
                };
                if let Err(error) = verification {
                    item_losses.push(artifact_error_loss(artifact, error)?);
                }
                if !item_losses.is_empty() {
                    losses.insert(key, item_losses);
                }
            }
            Ok(losses)
        })
    }
}

fn normalize_artifact(artifact: &mut RecoveryArtifactClosure) {
    if let RecoveryArtifactClosure::ValueInterface(reference) = artifact {
        reference.requirements.sort();
        reference.requirements.dedup();
    }
}
fn normalize_surface(surface: &mut RecoveryPublicSurface) {
    surface.compiler_context.normalize();
    surface.bindings.sort_by(|a, b| a.name.cmp(&b.name));
    surface.source_instances.sort_by(|a, b| {
        (
            &a.machine_incarnation,
            &a.instance,
            &a.module_version,
            &a.binder,
        )
            .cmp(&(
                &b.machine_incarnation,
                &b.instance,
                &b.module_version,
                &b.binder,
            ))
    });
}
fn normalize_node(node: &mut RecoveryNode) {
    node.implementation_refs.sort();
    node.implementation_refs.dedup();
    node.lexical_roots.sort();
    node.lexical_roots.dedup();
    node.lexical.sort_by(|a, b| a.owner.cmp(&b.owner));
    for lexical in &mut node.lexical {
        lexical.imports.sort();
        lexical.imports.dedup();
    }
    node.artifact_refs.sort();
    node.artifact_refs.dedup();
    node.native_groups.sort();
    node.native_groups.dedup();
    node.compiler_roles.sort_by_key(|role| role.interface());
}

impl RecoveryGraphWire {
    pub(crate) fn empty(source_session: u64, lineage: u64) -> Result<Self, RecoveryError> {
        let mut graph = Self {
            version: VERSION,
            public_schema: PAIRED_PUBLIC_SCHEMA.into(),
            source_session,
            lineage,
            high_water: Generation(0),
            public_surfaces: Vec::new(),
            nodes: Vec::new(),
            artifacts: Vec::new(),
            artifact_dependencies: Vec::new(),
            checksum: String::new(),
        };
        graph.seal()?;
        Ok(graph)
    }

    /// Fill the digest after the owner has assembled a complete graph.
    pub(crate) fn seal(&mut self) -> Result<(), RecoveryError> {
        self.version = VERSION;
        self.public_schema = PAIRED_PUBLIC_SCHEMA.into();
        self.nodes.sort_by_key(|node| node.id);
        self.artifacts
            .sort_by_key(RecoveryArtifactClosure::artifact_id);
        self.artifact_dependencies
            .sort_by_key(|edge| (edge.source, edge.target, edge.dependency.clone()));
        self.artifact_dependencies.dedup();
        self.public_surfaces.sort_by(|a, b| a.owner.cmp(&b.owner));
        for row in &mut self.nodes {
            normalize_node(row);
        }
        for row in &mut self.artifacts {
            normalize_artifact(row);
        }
        for row in &mut self.public_surfaces {
            normalize_surface(row);
        }
        self.checksum.clear();
        validate_shape(self)?;
        self.checksum = checksum(self)?;
        Ok(())
    }

    pub(crate) fn validate(&self) -> Result<(), RecoveryError> {
        validate_shape(self)?;
        if self.checksum != checksum(self)? {
            return Err(error("recovery graph checksum mismatch"));
        }
        Ok(())
    }
}

fn validate_shape(graph: &impl GraphRead) -> Result<(), RecoveryError> {
    if graph.version() != VERSION {
        return Err(error(format!(
            "unsupported recovery version {}",
            graph.version()
        )));
    }
    if graph.public_schema() != PAIRED_PUBLIC_SCHEMA {
        return Err(error("unsupported paired public visibility schema"));
    }
    let mut public_owners = BTreeSet::new();
    for surface in graph.public_surfaces() {
        if tidepool_repr::ActorPath::parse(&surface.owner.path).is_err()
            || surface.owner.incarnation == 0
            || !public_owners.insert(&surface.owner)
        {
            return Err(error("invalid or duplicate public actor owner"));
        }
        let lexical = &surface.compiler_context.lexical;
        let owners = lexical
            .iter()
            .map(|row| &row.owner)
            .collect::<BTreeSet<_>>();
        if owners.len() != lexical.len()
            || lexical.iter().any(|row| {
                row.owner.unit.is_empty()
                    || row.owner.module.is_empty()
                    || row.imports.iter().collect::<BTreeSet<_>>().len() != row.imports.len()
                    || row.imports.iter().any(|import| !owners.contains(import))
            })
        {
            return Err(error("invalid public source lexical selection"));
        }
        let mut binding_names = BTreeSet::new();
        for binding in &surface.bindings {
            if binding.name.is_empty()
                || binding.name.contains('\n')
                || binding.name.contains('\r')
                || binding.owner.session == 0
                || !binding_names.insert(&binding.name)
            {
                return Err(error("invalid or duplicate public binding winner"));
            }
        }
        let mut source_instances = BTreeSet::new();
        for source in &surface.source_instances {
            if source.machine_incarnation == 0
                || source.binder.unit.is_empty()
                || source.binder.module.is_empty()
                || source.binder.namespace.is_empty()
                || source.binder.occurrence.is_empty()
                || !source_instances.insert((
                    source.machine_incarnation,
                    source.instance,
                    &source.module_version,
                    &source.binder,
                ))
            {
                return Err(error("invalid or duplicate public source instance"));
            }
        }
        if surface.epoch == 0
            && (!surface.bindings.is_empty() || !surface.source_instances.is_empty())
        {
            return Err(error("public winners require a visibility epoch"));
        }
    }
    if graph.lineage() == 0 {
        return Err(error("recovery lineage must be nonzero"));
    }
    let mut nodes = BTreeMap::new();
    for node in graph.nodes() {
        if node.id.0 == 0 || node.id > graph.high_water() || nodes.insert(node.id, node).is_some() {
            return Err(error(format!(
                "invalid or duplicate recovery node {}",
                node.id.0
            )));
        }
        if node.parent.is_some_and(|parent| parent >= node.id)
            || node.implementation_refs.contains(&node.id)
        {
            return Err(error(format!(
                "recovery node {} has an invalid parent or self implementation reference",
                node.id.0
            )));
        }
        if matches!(&node.state, RecoveryNodeState::ExactArtifactClosure)
            && !node.live_dependencies.is_empty()
        {
            return Err(error(format!(
                "recovery node {} claims exact recovery with live dependencies",
                node.id.0
            )));
        }
        if !node.live_dependencies.is_empty()
            && !matches!(&node.state, RecoveryNodeState::LiveValueDependency { .. })
        {
            return Err(error(format!(
                "recovery node {} must report its live-value dependencies",
                node.id.0
            )));
        }
        let lexical_owners: BTreeMap<_, _> = node
            .lexical
            .iter()
            .map(|entry| (&entry.owner, &entry.imports))
            .collect();
        let mut unique_roots = BTreeSet::new();
        if node.lexical_roots.iter().any(|root| {
            root.unit.is_empty()
                || root.module.is_empty()
                || !unique_roots.insert(root)
                || !lexical_owners.contains_key(root)
        }) {
            return Err(error(format!(
                "recovery node {} has invalid or unrepresented lexical roots",
                node.id.0
            )));
        }
        let mut unique_owners = BTreeSet::new();
        for lexical in &node.lexical {
            let mut unique_imports = BTreeSet::new();
            if lexical.owner.unit.is_empty()
                || lexical.owner.module.is_empty()
                || !unique_owners.insert(&lexical.owner)
                || lexical.imports.iter().any(|import| {
                    import.unit.is_empty()
                        || import.module.is_empty()
                        || !unique_imports.insert(import)
                        || !lexical_owners.contains_key(import)
                })
            {
                return Err(error(format!(
                    "recovery node {} has invalid or non-closed lexical edges",
                    node.id.0
                )));
            }
        }
        if !node.lexical.is_empty() {
            let mut reachable = BTreeSet::new();
            let mut pending = node.lexical_roots.clone();
            while let Some(owner) = pending.pop() {
                if reachable.insert(owner.clone()) {
                    pending.extend(lexical_owners[&owner].iter().cloned());
                }
            }
            if reachable.len() != lexical_owners.len() {
                return Err(error(format!(
                    "recovery node {} retains lexical owners outside its exact root closure",
                    node.id.0
                )));
            }
        } else if !node.lexical_roots.is_empty() {
            return Err(error(format!(
                "recovery node {} has lexical roots without edges",
                node.id.0
            )));
        }
        if node.workbench_imports.iter().any(|spec| {
            let trimmed = spec.trim();
            trimmed.is_empty()
                || trimmed != spec
                || spec.contains('\n')
                || spec.contains('\r')
                || spec
                    .strip_prefix("import")
                    .is_some_and(|rest| rest.chars().next().is_some_and(char::is_whitespace))
        }) {
            return Err(error(format!(
                "recovery node {} has invalid workbench import metadata",
                node.id.0
            )));
        }
        let mut exports = BTreeSet::new();
        if node.exports.iter().any(|export| {
            !valid_identity(&export.identity)
                || !exports.insert(&export.identity)
                || export.children.iter().any(|child| !valid_identity(child))
        }) {
            return Err(error(format!(
                "recovery node {} has an invalid or duplicate export",
                node.id.0
            )));
        }
        if node.retracts.iter().any(|id| !valid_identity(id)) {
            return Err(error(format!(
                "recovery node {} has an invalid retraction",
                node.id.0
            )));
        }
        let families: BTreeSet<_> = node.instances.family_consistency_closure.iter().collect();
        let selected: BTreeSet<_> = node.instances.selected_family_axioms.iter().collect();
        if families.len() != node.instances.family_consistency_closure.len()
            || selected.len() != node.instances.selected_family_axioms.len()
            || !selected.is_subset(&families)
            || families.iter().any(|id| !valid_identity(id))
        {
            return Err(error(format!(
                "recovery node {} has invalid family evidence",
                node.id.0
            )));
        }
        let mut dfuns = BTreeSet::new();
        for instance in &node.instances.classes {
            if !valid_identity(&instance.dfun)
                || !valid_identity(&instance.class)
                || !dfuns.insert(&instance.dfun)
                || instance
                    .selected_axioms
                    .iter()
                    .any(|id| !selected.contains(id))
            {
                return Err(error(format!(
                    "recovery node {} has invalid instance evidence",
                    node.id.0
                )));
            }
        }
    }
    for node in graph.nodes() {
        if node.parent.is_some_and(|id| !nodes.contains_key(&id))
            || node
                .implementation_refs
                .iter()
                .any(|id| !nodes.contains_key(id))
        {
            return Err(error(format!(
                "recovery node {} references a missing node",
                node.id.0
            )));
        }
    }
    let mut incoming = nodes
        .keys()
        .map(|id| (*id, 0usize))
        .collect::<BTreeMap<_, _>>();
    for node in graph.nodes() {
        for dependency in node.parent.iter().chain(node.implementation_refs.iter()) {
            *incoming
                .get_mut(dependency)
                .expect("references were validated above") += 1;
        }
    }
    let mut ready = incoming
        .iter()
        .filter_map(|(id, count)| (*count == 0).then_some(*id))
        .collect::<VecDeque<_>>();
    let mut visited = 0usize;
    while let Some(id) = ready.pop_front() {
        visited += 1;
        let node = nodes[&id];
        for dependency in node.parent.iter().chain(node.implementation_refs.iter()) {
            let count = incoming.get_mut(dependency).expect("known recovery node");
            *count -= 1;
            if *count == 0 {
                ready.push_back(*dependency);
            }
        }
    }
    if visited != nodes.len() {
        return Err(error(
            "recovery parent and implementation references contain a cycle",
        ));
    }
    for surface in graph.public_surfaces() {
        if surface
            .declaration_root
            .is_some_and(|root| !nodes.contains_key(&root))
        {
            return Err(error("recovery public root is missing"));
        }
        if surface
            .declaration_root
            .is_some_and(|root| root > graph.high_water())
        {
            return Err(error("recovery public root exceeds high-water"));
        }
    }

    let mut artifacts = BTreeMap::new();
    for artifact in graph.artifacts() {
        let owner = artifact.owner();
        if owner.unit.is_empty()
            || owner.module.is_empty()
            || artifact
                .component_paths()
                .iter()
                .any(|(_, path)| validate_relative(path).is_err())
        {
            return Err(error("invalid recovery artifact reference"));
        }
        if artifact.declared_artifact_id() != artifact.artifact_id() {
            return Err(error("recovery artifact ID does not match its descriptor"));
        }
        if artifacts.insert(artifact.artifact_id(), artifact).is_some() {
            return Err(error("duplicate recovery artifact reference"));
        }
        if let RecoveryArtifactClosure::ValueInterface(reference) = artifact {
            let requirements = reference.requirements.iter().collect::<BTreeSet<_>>();
            if requirements.len() != reference.requirements.len() {
                return Err(error("duplicate recovery value interface requirement"));
            }
        }
    }
    for artifact in graph.artifacts() {
        if let RecoveryArtifactClosure::Home(reference) = artifact {
            let interface = reference.module_interface.as_ref().ok_or_else(|| {
                error("native recovery artifact lacks canonical module interface")
            })?;
            let canonical = ArtifactDescriptor::from_recovery_module_interface(interface);
            if canonical.owner != artifact.owner()
                || !matches!(
                    artifacts.get(&canonical.id),
                    Some(RecoveryArtifactClosure::ModuleInterface(_))
                )
                || !graph.artifact_dependencies().any(|edge| {
                    edge.source == artifact.artifact_id()
                        && edge.target == canonical.id
                        && edge.dependency == ArtifactDependency::Interface
                })
            {
                return Err(error(
                    "native recovery artifact lacks its canonical interface closure",
                ));
            }
        }
    }
    let mut unique_edges = BTreeSet::new();
    for edge in graph.artifact_dependencies() {
        if !unique_edges.insert((edge.source, edge.target, &edge.dependency)) {
            return Err(error("duplicate recovery artifact dependency"));
        }

        if !matches!(edge.dependency, ArtifactDependency::Interface) {
            return Err(error(
                "native recovery edges must derive from original certification",
            ));
        }
        if !artifacts.contains_key(&edge.source) || !artifacts.contains_key(&edge.target) {
            return Err(error(
                "recovery artifact dependency references a missing artifact",
            ));
        }
    }
    let selections = graph
        .nodes()
        .map(|node| {
            (
                Some(node),
                &node.artifact_refs,
                &node.native_groups,
                &node.compiler_roles,
                format!("recovery node {}", node.id.0),
            )
        })
        .chain(graph.public_surfaces().map(|surface| {
            (
                None,
                &surface.compiler_context.artifact_refs,
                &surface.compiler_context.native_groups,
                &surface.compiler_context.compiler_roles,
                format!("public source surface {:?}", surface.owner),
            )
        }));
    for (node, artifact_refs, selected_groups, compiler_roles, label) in selections {
        let node_artifacts = artifact_refs.iter().copied().collect::<BTreeSet<_>>();
        let mut native_groups = BTreeSet::new();
        for group in selected_groups {
            if !native_groups.insert(*group) {
                return Err(error("duplicate recovered native group selection"));
            }
            if !node_artifacts.contains(&group.artifact)
                || !matches!(
                    artifacts.get(&group.artifact),
                    Some(RecoveryArtifactClosure::Home(_))
                )
            {
                return Err(error(
                    "recovered native group lacks its exact original carrier",
                ));
            }
        }
        if artifact_refs.iter().any(|key| !artifacts.contains_key(key)) {
            return Err(error(format!("{} references a missing artifact", label)));
        }
        for id in &node_artifacts {
            if let RecoveryArtifactClosure::Home(reference) = artifacts[id] {
                let canonical = ArtifactDescriptor::from_recovery_module_interface(
                    reference
                        .module_interface
                        .as_ref()
                        .expect("validated native companion"),
                )
                .id;
                if !node_artifacts.contains(&canonical) {
                    return Err(error(format!(
                        "{} omits its canonical interface companion",
                        label
                    )));
                }
            }
        }
        let mut artifact_owners = BTreeMap::new();
        for id in &node_artifacts {
            let artifact = artifacts[id];
            let canonical = match artifact {
                RecoveryArtifactClosure::Home(home) => {
                    ArtifactDescriptor::from_recovery_module_interface(
                        home.module_interface
                            .as_ref()
                            .expect("validated native companion"),
                    )
                    .id
                }
                _ => *id,
            };
            if artifact_owners
                .insert(artifact.owner(), canonical)
                .is_some_and(|previous| previous != canonical)
            {
                return Err(error(format!(
                    "{} has conflicting canonical interfaces for one module owner",
                    label
                )));
            }
        }
        let mut compiler_owners = BTreeSet::new();
        for role in compiler_roles {
            let interface = role.interface();
            let Some(interface_artifact) = artifacts.get(&interface) else {
                return Err(error("recovered compiler role has no interface carrier"));
            };
            if !node_artifacts.contains(&interface)
                || matches!(interface_artifact, RecoveryArtifactClosure::Home(_))
                || !compiler_owners.insert(interface_artifact.owner())
            {
                return Err(error("duplicate compiler owner or invalid interface role"));
            }
            if let Some(original) = role.original() {
                let Some(RecoveryArtifactClosure::Home(home)) = artifacts.get(&original) else {
                    return Err(error(
                        "recovered compiler original offer has no native carrier",
                    ));
                };
                if !node_artifacts.contains(&original)
                    || ArtifactDescriptor::from_recovery_module_interface(
                        home.module_interface
                            .as_ref()
                            .expect("validated native companion"),
                    )
                    .id != interface
                {
                    return Err(error(
                        "recovered compiler original offer differs from its exact interface",
                    ));
                }
            }
        }
        for dependency in node.into_iter().flat_map(|node| &node.live_dependencies) {
            let RecoveryLiveDependency::NativeBinding {
                artifact_id,
                binding,
                generation: _,
            } = dependency
            else {
                return Err(error(
                    "unsupported recovery instance live dependency marker",
                ));
            };
            let artifact = artifacts
                .get(artifact_id)
                .filter(|_| node_artifacts.contains(artifact_id))
                .ok_or_else(|| {
                    error("native binding dependency has no retained exact interface")
                })?;
            let owner = artifact.owner();
            if owner.unit != binding.unit || owner.module != binding.module {
                return Err(error(
                    "native binding dependency differs from its certified original identity",
                ));
            }
        }
        for id in &node_artifacts {
            let RecoveryArtifactClosure::ValueInterface(reference) = artifacts[id] else {
                continue;
            };
            for requirement in &reference.requirements {
                let target = artifact_owners.get(requirement).ok_or_else(|| {
                    error(format!(
                        "{} has a value interface with a missing exact module requirement",
                        label
                    ))
                })?;
                if !graph.artifact_dependencies().any(|edge| {
                    edge.source == reference.artifact_id
                        && edge.target == *target
                        && edge.dependency == ArtifactDependency::Interface
                }) {
                    return Err(error(format!(
                        "{} has a value interface requirement without its direct artifact edge",
                        label
                    )));
                }
            }
        }
        if graph.artifact_dependencies().any(|edge| {
            node_artifacts.contains(&edge.source) && !node_artifacts.contains(&edge.target)
        }) {
            return Err(error(format!(
                "{} has an incomplete artifact dependency closure",
                label
            )));
        }
        if node.is_some_and(|node| {
            matches!(&node.state, RecoveryNodeState::ExactArtifactClosure)
                && artifact_refs.is_empty()
                && !node.exports.is_empty()
        }) {
            return Err(error(format!(
                "{} has exports but no exact artifact closure",
                label
            )));
        }
    }
    let referenced_artifacts: BTreeSet<_> = graph
        .nodes()
        .flat_map(|node| node.artifact_refs.iter().copied())
        .chain(
            graph
                .public_surfaces()
                .flat_map(|surface| surface.compiler_context.artifact_refs.iter().copied()),
        )
        .collect();
    let owned_modules: BTreeSet<_> = referenced_artifacts
        .iter()
        .filter_map(|key| artifacts.get(key).copied())
        .map(|artifact| artifact.owner())
        .collect();
    for node in graph.nodes() {
        if node
            .lexical
            .iter()
            .any(|lexical| !owned_modules.contains(&lexical.owner))
        {
            return Err(error(format!(
                "recovery node {} has lexical ownership without a referenced exact artifact",
                node.id.0
            )));
        }
    }
    for surface in graph.public_surfaces() {
        let selection = &surface.compiler_context;
        let selected_owners = selection
            .artifact_refs
            .iter()
            .filter_map(|id| artifacts.get(id))
            .map(|artifact| artifact.owner())
            .collect::<BTreeSet<_>>();
        if selection
            .lexical
            .iter()
            .any(|row| !selected_owners.contains(&row.owner))
        {
            return Err(error(
                "public source lexical owner lacks its selected exact artifact",
            ));
        }
    }
    Ok(())
}

fn checksum(graph: &impl GraphRead) -> Result<String, RecoveryError> {
    checksum_with_encoded_bytes(graph).map(|(checksum, _)| checksum)
}

fn checksum_with_encoded_bytes(graph: &impl GraphRead) -> Result<(String, u64), RecoveryError> {
    let bytes = serde_json::to_vec(&GraphEncoding {
        graph,
        checksum: "",
    })
    .map_err(|e| error(format!("could not encode recovery graph: {e}")))?;
    let mut domain = b"tidepool-recovery-graph-v9\0".to_vec();
    domain.extend_from_slice(&bytes);
    Ok((
        blake3::hash(&domain).to_hex().to_string(),
        bytes.len() as u64,
    ))
}

fn valid_identity(identity: &RecoverySymbolIdentity) -> bool {
    !identity.unit.is_empty()
        && !identity.module.is_empty()
        && !identity.namespace.is_empty()
        && !identity.occurrence.is_empty()
        && identity.record_parent.as_deref().is_none_or(valid_identity)
}

fn validate_relative(path: &Path) -> Result<(), RecoveryError> {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(error(format!("unsafe artifact path {}", path.display())));
    }
    Ok(())
}

fn artifact_error_loss(
    artifact: &RecoveryArtifactClosure,
    error: RecoveryArtifactError,
) -> Result<RecoveryArtifactLoss, RecoveryError> {
    let (component, path, kind) = match error {
        RecoveryArtifactError::CompletedSourceEvidence { input, failure } => {
            return Err(RecoveryError {
                path: Some(input.clone()),
                detail: format!("completed source evidence refused: {failure:?}"),
                kind: RecoveryErrorKind::SourceEvidence {
                    input,
                    failure: *failure,
                },
            });
        }
        RecoveryArtifactError::InventoryAccounting(cause) => {
            // Exhausted admission accounting says nothing about the immutable
            // artifact's validity. Abort recovery rather than tombstoning it.
            return Err(RecoveryError {
                path: None,
                detail: format!("recovery inventory admission refused: {cause}"),
                kind: RecoveryErrorKind::InventoryAccounting(cause),
            });
        }
        RecoveryArtifactError::ExecutionSourceProducerMismatch {
            unit,
            module,
            expected,
            actual,
        } => {
            let (interface, product) = artifact.paths();
            let (component, path) = match product {
                Some(product) => (RecoveryArtifactComponent::Product, product),
                None => (RecoveryArtifactComponent::Interface, interface),
            };
            (
                component,
                path.to_path_buf(),
                RecoveryArtifactLossKind::ExecutionSourceProducerMismatch {
                    unit,
                    module,
                    expected,
                    actual,
                },
            )
        }
        RecoveryArtifactError::UnsupportedPackageImportsVersion { path, found } => {
            let (component, relative) = artifact_component(artifact, &path);
            (
                component,
                relative,
                RecoveryArtifactLossKind::UnsupportedPackageImportsVersion { found },
            )
        }
        RecoveryArtifactError::Unavailable(path) => {
            let (component, relative) = artifact_component(artifact, &path);
            (component, relative, RecoveryArtifactLossKind::Missing)
        }
        RecoveryArtifactError::DigestMismatch(path) => {
            let (component, relative) = artifact_component(artifact, &path);
            (
                component,
                relative,
                RecoveryArtifactLossKind::DigestMismatch,
            )
        }
        RecoveryArtifactError::InvalidPackageImports(path)
        | RecoveryArtifactError::InvalidCertifiedOwners(path)
        | RecoveryArtifactError::InvalidCapturedPayload(path)
        | RecoveryArtifactError::InvalidModuleCertificate(path) => {
            let (component, relative) = artifact_component(artifact, &path);
            (
                component,
                relative,
                RecoveryArtifactLossKind::Unreadable("invalid artifact witness".into()),
            )
        }
        RecoveryArtifactError::CertifiedOwnersUnavailable(path) => {
            let (component, relative) = artifact_component(artifact, &path);
            (component, relative, RecoveryArtifactLossKind::Missing)
        }
        RecoveryArtifactError::CertifiedOwnersDigestMismatch(path) => {
            let (component, relative) = artifact_component(artifact, &path);
            (
                component,
                relative,
                RecoveryArtifactLossKind::DigestMismatch,
            )
        }
        RecoveryArtifactError::Unreadable { path, error } => {
            let (component, relative) = artifact_component(artifact, &path);
            (
                component,
                relative,
                RecoveryArtifactLossKind::Unreadable(error.to_string()),
            )
        }
        RecoveryArtifactError::Io(error) => {
            let (interface, _) = artifact.paths();
            (
                RecoveryArtifactComponent::Interface,
                interface.to_path_buf(),
                RecoveryArtifactLossKind::Unreadable(error.to_string()),
            )
        }
        RecoveryArtifactError::InvalidReference => {
            let (interface, _) = artifact.paths();
            (
                RecoveryArtifactComponent::Interface,
                interface.to_path_buf(),
                RecoveryArtifactLossKind::Unreadable("invalid artifact reference".into()),
            )
        }
    };
    Ok(RecoveryArtifactLoss {
        component,
        path,
        kind,
    })
}

fn artifact_component(
    artifact: &RecoveryArtifactClosure,
    path: &Path,
) -> (RecoveryArtifactComponent, PathBuf) {
    let paths = artifact.component_paths();
    // A verifier may report an absolute root-qualified path. Match the longest
    // complete relative payload path to avoid confusing overlapping suffixes.
    if let Some((component, relative)) = paths
        .into_iter()
        .filter(|(_, relative)| path.ends_with(relative))
        .max_by_key(|(_, relative)| relative.components().count())
    {
        (component, relative.to_path_buf())
    } else {
        // Owned payload errors name their relative descriptor under the root.
        // Remaining path-bearing errors come from package interfaces selected
        // by the authenticated package witness, which may live outside it.
        (
            RecoveryArtifactComponent::ExternalPackageInterface,
            path.to_path_buf(),
        )
    }
}

fn node_recoverability(
    id: Generation,
    nodes: &BTreeMap<Generation, &RecoveryNode>,
    artifact_losses: &BTreeMap<ArtifactId, Vec<RecoveryArtifactLoss>>,
    availability: RecoveryAvailability,
    memo: &mut BTreeMap<Generation, Result<(), RecoveryLossReason>>,
) -> Result<(), RecoveryLossReason> {
    if let Some(result) = memo.get(&id) {
        return result.clone();
    }
    let Some(node) = nodes.get(&id).copied() else {
        return Err(RecoveryLossReason::MissingArtifactClosure(
            "referenced declaration node is absent".into(),
        ));
    };
    let result = match &node.state {
        RecoveryNodeState::LiveValueDependency { reason }
            if availability == RecoveryAvailability::Native =>
        {
            Err(RecoveryLossReason::LiveValueDependency(reason.clone()))
        }
        RecoveryNodeState::MissingArtifactClosure { reason } => {
            Err(RecoveryLossReason::MissingArtifactClosure(reason.clone()))
        }
        RecoveryNodeState::ExactArtifactClosure | RecoveryNodeState::LiveValueDependency { .. } => {
            if availability == RecoveryAvailability::Native && !node.live_dependencies.is_empty() {
                Err(RecoveryLossReason::LiveValueDependency(format!(
                    "{} live dependency record(s)",
                    node.live_dependencies.len()
                )))
            } else {
                let mut failure = None;
                for key in &node.artifact_refs {
                    if let Some(items) = artifact_losses.get(key) {
                        if let Some(loss) = items.first() {
                            failure = Some(RecoveryLossReason::Artifact(loss.clone()));
                            break;
                        }
                    }
                }
                if let Some(reason) = failure {
                    Err(reason)
                } else {
                    let dependencies = node
                        .parent
                        .into_iter()
                        .chain(node.implementation_refs.iter().copied());
                    let mut failure = None;
                    for dependency in dependencies {
                        if let Err(reason) = node_recoverability(
                            dependency,
                            nodes,
                            artifact_losses,
                            availability,
                            memo,
                        ) {
                            failure = Some(RecoveryLossReason::Dependency {
                                generation: dependency,
                                detail: format!("{reason:?}"),
                            });
                            break;
                        }
                    }
                    failure.map_or(Ok(()), Err)
                }
            }
        }
    };
    memo.insert(id, result.clone());
    result
}

fn error(detail: impl Into<String>) -> RecoveryError {
    RecoveryError {
        path: None,
        detail: detail.into(),
        kind: RecoveryErrorKind::Manifest,
    }
}
fn at(path: &Path, detail: impl Into<String>) -> RecoveryError {
    RecoveryError {
        path: Some(path.to_path_buf()),
        detail: detail.into(),
        kind: RecoveryErrorKind::Manifest,
    }
}

fn refusal_at(path: &Path, refusal: RecoveryRefusal) -> RecoveryError {
    let detail = match refusal {
        RecoveryRefusal::UnsupportedOldFormat { version } => {
            format!("unsupported old recovery manifest version {version}")
        }
        RecoveryRefusal::UnsupportedFutureFormat { version } => {
            format!("unsupported future recovery manifest version {version}")
        }
    };
    RecoveryError {
        path: Some(path.to_path_buf()),
        detail,
        kind: RecoveryErrorKind::Format(refusal),
    }
}

mod generation_serde {
    use serde::{Deserialize, Deserializer, Serializer};
    use tidepool_repr::Generation;
    pub fn serialize<S: Serializer>(value: &Generation, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u64(value.0)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Generation, D::Error> {
        u64::deserialize(deserializer).map(Generation)
    }
}

mod option_generation_serde {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use tidepool_repr::Generation;
    pub fn serialize<S: Serializer>(
        value: &Option<Generation>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        value.map(|generation| generation.0).serialize(serializer)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<Generation>, D::Error> {
        Option::<u64>::deserialize(deserializer).map(|value| value.map(Generation))
    }
}

mod generation_vec_serde {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use tidepool_repr::Generation;
    pub fn serialize<S: Serializer>(
        value: &[Generation],
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        value
            .iter()
            .map(|generation| generation.0)
            .collect::<Vec<_>>()
            .serialize(serializer)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Vec<Generation>, D::Error> {
        Vec::<u64>::deserialize(deserializer)
            .map(|values| values.into_iter().map(Generation).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owner(path: &str) -> RecoveryPublicOwner {
        RecoveryPublicOwner::new(&tidepool_repr::ActorPath::parse(path).unwrap(), 1).unwrap()
    }

    fn identity(occurrence: &str) -> RecoverySymbolIdentity {
        RecoverySymbolIdentity {
            unit: "main".into(),
            module: "Lib".into(),
            namespace: "value".into(),
            occurrence: occurrence.into(),
            record_parent: None,
        }
    }

    fn identity_in(module: &str, occurrence: &str) -> RecoverySymbolIdentity {
        RecoverySymbolIdentity {
            unit: "main".into(),
            module: module.into(),
            namespace: "value".into(),
            occurrence: occurrence.into(),
            record_parent: None,
        }
    }

    fn module(unit: &str, module: &str) -> ExactModuleIdentity {
        ExactModuleIdentity {
            unit: unit.into(),
            module: module.into(),
        }
    }

    #[test]
    fn public_compiler_context_normalization_preserves_duplicate_refusals() {
        let mut valid = fixture();
        let lexical_owner = home_artifact(&valid).owner();
        let selected = &valid.nodes[0];
        valid.public_surfaces[0].compiler_context = RecoveryCompilerContext {
            artifact_refs: selected.artifact_refs.clone(),
            native_groups: selected.native_groups.clone(),
            compiler_roles: selected.compiler_roles.clone(),
            lexical: vec![ExactLexicalNode {
                owner: lexical_owner.clone(),
                imports: vec![lexical_owner],
            }],
        };
        valid.seal().unwrap();
        for duplicate_node in [false, true] {
            let mut malformed = valid.clone();
            let lexical = &mut malformed.public_surfaces[0].compiler_context.lexical;
            let repeated = lexical[0].clone();
            if duplicate_node {
                lexical.push(repeated);
            } else {
                let consumer = lexical
                    .iter_mut()
                    .find(|row| !row.imports.is_empty())
                    .unwrap();
                consumer.imports.push(consumer.imports[0].clone());
            }
            assert!(malformed.seal().is_err());
        }
    }

    proptest::proptest! {
        #![proptest_config({
            let mut config = proptest::test_runner::Config::default();
            if let Some(path) = option_env!("TIDEPOOL_PROPTEST_REGRESSIONS") {
                config.failure_persistence = Some(Box::new(proptest::test_runner::FileFailurePersistence::Direct(path)));
            }
            config
        })]
        #[test]
        fn public_compiler_context_normalization_preserves_exact_selection(
            rows in proptest::collection::vec((proptest::num::u16::ANY, proptest::num::u8::ANY, proptest::bool::ANY), 0..9)
        ) {
            let owners = (0..rows.len())
                .map(|index| module(if index % 2 == 0 { "main" } else { "other" }, &format!("M{}", index / 2)))
                .collect::<Vec<_>>();
            let mut lexical = rows.iter().enumerate().map(|(index, (_, mask, reverse))| {
                let mut imports = owners.iter().enumerate()
                    .filter(|(target, _)| mask & (1u8 << target) != 0)
                    .map(|(_, owner)| owner.clone()).collect::<Vec<_>>();
                if *reverse { imports.reverse(); }
                (rows[index].0, ExactLexicalNode { owner: owners[index].clone(), imports })
            }).collect::<Vec<_>>();
            lexical.sort_by_key(|(rank, _)| *rank);
            let mut context = RecoveryCompilerContext {
                artifact_refs: (0..rows.len()).rev().map(|index| ArtifactId([index as u8; 32])).collect(),
                native_groups: (0..rows.len()).rev().map(|index| tidepool_toolchain::artifact_inventory::NativeGroupKey {
                    artifact: ArtifactId([index as u8; 32]), original_ordinal: index as u32,
                }).collect(),
                compiler_roles: (0..rows.len()).map(|index| tidepool_toolchain::artifact_inventory::CompilerInputRole::InterfaceOnly {
                    interface: ArtifactId([index as u8; 32]),
                }).collect(),
                lexical: lexical.into_iter().map(|(_, node)| node).collect(),
            };
            // This metadata-only model confers no compiler authority.
            let model = context.lexical.iter().map(|node| (
                node.owner.clone(), node.imports.iter().cloned().collect::<BTreeSet<_>>()
            )).collect::<BTreeMap<_, _>>();
            let artifact_set = context.artifact_refs.iter().cloned().collect::<BTreeSet<_>>();
            let native_set = context.native_groups.iter().cloned().collect::<BTreeSet<_>>();
            let roles = context.compiler_roles.clone();
            context.normalize();
            proptest::prop_assert_eq!(&context.lexical, &model.into_iter().map(|(owner, imports)| ExactLexicalNode {
                owner, imports: imports.into_iter().collect(),
            }).collect::<Vec<_>>());
            proptest::prop_assert_eq!(&context.artifact_refs, &artifact_set.into_iter().collect::<Vec<_>>());
            proptest::prop_assert_eq!(&context.native_groups, &native_set.into_iter().collect::<Vec<_>>());
            proptest::prop_assert_eq!(&context.compiler_roles, &roles);
            let canonical = serde_json::to_vec(&context).unwrap();
            let mut reordered = context.clone();
            reordered.lexical.reverse();
            for node in &mut reordered.lexical { node.imports.reverse(); }
            reordered.artifact_refs.reverse();
            reordered.native_groups.reverse();
            reordered.normalize();
            proptest::prop_assert_eq!(serde_json::to_vec(&reordered).unwrap(), canonical);
            reordered.normalize();
            proptest::prop_assert_eq!(&reordered, &context);
            reordered.native_groups.push(tidepool_toolchain::artifact_inventory::NativeGroupKey {
                artifact: ArtifactId([255; 32]), original_ordinal: 9001,
            });
            reordered.normalize();
            proptest::prop_assert_ne!(reordered, context);
        }
    }

    fn snapshot(wire: &RecoveryGraphWire) -> RecoveryGraph {
        RecoveryGraph::from_wire(wire.clone()).unwrap()
    }

    fn home_artifact(wire: &RecoveryGraphWire) -> &RecoveryArtifactClosure {
        wire.artifacts
            .iter()
            .find(|artifact| matches!(artifact, RecoveryArtifactClosure::Home(_)))
            .unwrap()
    }

    fn home_artifact_mut(wire: &mut RecoveryGraphWire) -> &mut RecoveryArtifactClosure {
        wire.artifacts
            .iter_mut()
            .find(|artifact| matches!(artifact, RecoveryArtifactClosure::Home(_)))
            .unwrap()
    }

    fn install_fixture_canonical_interfaces(wire: &mut RecoveryGraphWire) {
        let companions = wire
            .artifacts
            .iter()
            .filter_map(|artifact| {
                let RecoveryArtifactClosure::Home(home) = artifact else {
                    return None;
                };
                let interface = RecoveryArtifactClosure::ModuleInterface(
                    home.module_interface.clone().unwrap(),
                );
                Some((artifact.artifact_id(), interface))
            })
            .collect::<Vec<_>>();
        for (native, interface) in companions {
            let canonical = interface.artifact_id();
            if !wire
                .artifacts
                .iter()
                .any(|artifact| artifact.artifact_id() == canonical)
            {
                wire.artifacts.push(interface);
            }
            wire.artifact_dependencies.push(RecoveryArtifactDependency {
                source: native,
                target: canonical,
                dependency: ArtifactDependency::Interface,
            });
            for node in &mut wire.nodes {
                if node.artifact_refs.contains(&native) {
                    node.artifact_refs.push(canonical);
                    let role = tidepool_toolchain::artifact_inventory::CompilerInputRole::ReusableOriginal {
                        interface: canonical,
                        original: native,
                    };
                    if !node.compiler_roles.contains(&role) {
                        node.compiler_roles.push(role);
                    }
                }
            }
        }
    }

    // Graph records describe custody and content identities; they issue no compiler
    // authority and deliberately have no materialized bytes behind these paths.
    fn fixture() -> RecoveryGraphWire {
        use tidepool_toolchain::recovery_artifacts::RecoveryCoreRef;

        let interface = RecoveryJoinRef {
            toolchain_identity_sha256: [0x11; 32],
            unit: "main".into(),
            module: "Lib".into(),
            skinny_iface_sha256: [0x33; 32],
            interface_path: "artifacts/Lib.hi".into(),
            package_imports_path: "artifacts/Lib.hi.packages".into(),
            package_imports_sha256: [0x44; 32],
        };
        graph_with_home(RecoveryArtifactRef {
            toolchain_identity_sha256: interface.toolchain_identity_sha256,
            unit: interface.unit.clone(),
            module: interface.module.clone(),
            module_version: [0x22; 32],
            skinny_iface_sha256: interface.skinny_iface_sha256,
            product_sha256: [0x55; 32],
            interface_path: interface.interface_path.clone(),
            package_imports_path: interface.package_imports_path.clone(),
            package_imports_sha256: interface.package_imports_sha256,
            certification_path: "artifacts/Lib.native-certificate".into(),
            certification_sha256: [0x66; 32],
            product_path: "artifacts/Lib.native".into(),
            module_interface: Some(RecoveryModuleInterfaceRef {
                interface,
                certificate_path: "artifacts/Lib.interface-certificate".into(),
                certificate_sha256: [0x77; 32],
                core: Some(RecoveryCoreRef {
                    path: "artifacts/Lib.core".into(),
                    sha256: [0x88; 32],
                    bytes: 1,
                }),
            }),
            execution_source: None,
        })
    }

    // Only the production compiler creates the certified bundles used by recovery
    // acceptance tests. Repeated installations retain immutable compiled bytes
    // while each test has its own mutable recovery directory.
    fn compiled_fixture(root: &Path) -> RecoveryGraphWire {
        use std::sync::OnceLock;
        use tidepool_toolchain::recovery_artifacts::{
            materialize_certified_products, CertifiedRecoveryProduct,
        };

        static PRODUCTS: OnceLock<(
            [u8; 32],
            Vec<CertifiedRecoveryProduct>,
            Vec<tidepool_toolchain::artifact_inventory::NativeGroupKey>,
        )> = OnceLock::new();
        let (producer, products, native_groups) = PRODUCTS.get_or_init(|| {
            tidepool_testing::eval_harness::require_extract();
            let compiled = tidepool_testing::with_settlement(|settlement| {
                tidepool_toolchain::artifacts::compile_targets(
                    include_str!("fixtures/recovery-control.hs"),
                    &["answer"],
                    &[],
                    |_, _, _| {},
                    settlement,
                )
            })
            .expect("production compilation of recovery control");
            let descriptors = compiled.artifact_view.descriptors();
            let producer = descriptors
                .iter()
                .find(|descriptor| {
                    descriptor.kind
                        == tidepool_toolchain::artifact_inventory::ArtifactKind::OriginalModule
                        && descriptor.owner.unit == "main"
                        && descriptor.owner.module == "Lib"
                })
                .expect("compiler-certified Lib artifact descriptor")
                .producer_sha256;
            let native_groups = compiled
                .artifact_view
                .selected_native_groups()
                .into_iter()
                .collect();
            let products = compiled
                .recovery_products
                .into_iter()
                .filter(|product| product.owner().unit == "main" && product.owner().module == "Lib")
                .collect::<Vec<_>>();
            assert_eq!(products.len(), 1, "compiler-issued Lib native product");
            (producer, products, native_groups)
        });
        let mut materialized = materialize_certified_products(root, *producer, products)
            .expect("materialize compiler-issued recovery control");
        let mut graph = graph_with_home(materialized.pop().unwrap());
        // compile_targets produces a source original, not an authored session
        // generation. The graph selects it as an implementation of a join.
        graph.nodes[0].kind = RecoveryNodeKind::Join;
        graph.nodes[0].native_groups = native_groups.clone();
        graph.seal().unwrap();
        assert!(snapshot(&graph)
            .validate_artifact_files(root)
            .unwrap()
            .is_empty());
        snapshot(&graph)
            .capture_inventory(root)
            .expect("compiler-issued recovery inventory");
        graph
    }

    struct CompiledGroupSelection {
        producer: [u8; 32],
        products: Vec<tidepool_toolchain::recovery_artifacts::CertifiedRecoveryProduct>,
        late: tidepool_toolchain::artifact_inventory::NativeGroupKey,
        late_closure: BTreeSet<tidepool_toolchain::artifact_inventory::NativeGroupKey>,
        zero_edge: tidepool_toolchain::artifact_inventory::NativeGroupKey,
    }

    fn compiled_group_selection(
        root: &Path,
    ) -> (RecoveryGraphWire, &'static CompiledGroupSelection) {
        use std::sync::OnceLock;
        use tidepool_toolchain::artifact_inventory::{ArtifactKind, NativeGroupKey};
        static FIXTURE: OnceLock<CompiledGroupSelection> = OnceLock::new();
        let fixture = FIXTURE.get_or_init(|| {
            tidepool_testing::eval_harness::require_extract();
            let compiled = tidepool_testing::with_settlement(|settlement| {
                tidepool_toolchain::artifacts::compile_targets(
                    include_str!("fixtures/recovery-group-selection.hs"),
                    &["early", "late", "isolated"],
                    &[],
                    |_, _, _| {},
                    settlement,
                )
            })
            .expect("production compilation of independent native groups");
            let descriptors = compiled.artifact_view.descriptors();
            let original = descriptors
                .iter()
                .find(|row| {
                    row.kind == ArtifactKind::OriginalModule
                        && row.owner.unit == "main"
                        && row.owner.module == "Lib"
                })
                .expect("actual original Lib carrier");
            let issued_root = |name: &str| {
                let matches = compiled
                    .certified_groups
                    .iter()
                    .filter(|group| {
                        group.owner().unit == "main"
                            && group.owner().module == "Lib"
                            && group
                                .group()
                                .binders()
                                .iter()
                                .any(|binder| binder.occurrence == name)
                    })
                    .collect::<Vec<_>>();
                assert_eq!(matches.len(), 1, "one actual original binder for {name}");
                NativeGroupKey {
                    artifact: original.id,
                    original_ordinal: matches[0].group().original_ordinal(),
                }
            };
            let edges = compiled
                .artifact_view
                .dependencies()
                .into_iter()
                .filter_map(|(from, to, edge)| match edge {
                    ArtifactDependency::NativeGroup {
                        dependent_ordinal,
                        required_ordinal,
                    } => Some((
                        NativeGroupKey {
                            artifact: from,
                            original_ordinal: dependent_ordinal,
                        },
                        NativeGroupKey {
                            artifact: to,
                            original_ordinal: required_ordinal,
                        },
                    )),
                    _ => None,
                })
                .collect::<Vec<_>>();
            // Independent fixed point over compiler-issued group relations; the
            // recovery primitive under test does not construct this expectation.
            let closure = |root| {
                let mut selected = BTreeSet::from([root]);
                loop {
                    let before = selected.len();
                    for (from, to) in &edges {
                        if selected.contains(from) {
                            selected.insert(*to);
                        }
                    }
                    if selected.len() == before {
                        return selected;
                    }
                }
            };
            let late = issued_root("late");
            let late_closure = closure(late);
            assert!(
                late_closure.is_superset(&closure(issued_root("early"))),
                "late genuinely retains the earlier original group"
            );
            assert!(
                late_closure.len() > 1,
                "missing dependency control must be non-vacuous"
            );
            let zero_edge = closure(issued_root("isolated"))
                .into_iter()
                .find(|group| {
                    !late_closure.contains(group) && !edges.iter().any(|(from, _)| from == group)
                })
                .expect("independent closed zero-edge original group");
            let products = compiled
                .recovery_products
                .into_iter()
                .filter(|product| product.owner().unit == "main" && product.owner().module == "Lib")
                .collect::<Vec<_>>();
            assert_eq!(products.len(), 1, "full compiler-issued Lib product");
            CompiledGroupSelection {
                producer: original.producer_sha256,
                products,
                late,
                late_closure,
                zero_edge,
            }
        });
        let mut materialized =
            tidepool_toolchain::recovery_artifacts::materialize_certified_products(
                root,
                fixture.producer,
                &fixture.products,
            )
            .unwrap();
        let mut wire = graph_with_home(materialized.pop().unwrap());
        wire.nodes[0].kind = RecoveryNodeKind::Join;
        wire.nodes[0].exports[0].identity = identity("late");
        wire.nodes[0].native_groups = fixture.late_closure.iter().copied().collect();
        wire.seal().unwrap();
        (wire, fixture)
    }

    #[test]
    fn v8_partial_native_group_selection_roundtrips_and_retains_zero_edge_groups() {
        let root = tempfile::tempdir().unwrap();
        let (wire, fixture) = compiled_group_selection(root.path());
        let baseline = snapshot(&wire);
        let inventory = baseline.capture_inventory(root.path()).unwrap();
        let node = baseline.node(Generation(1)).unwrap();
        let restored = inventory
            .context_with_roles(
                &node.artifact_refs,
                &node.native_groups,
                &node.compiler_roles,
                node.lexical.clone(),
            )
            .unwrap();
        assert_eq!(
            restored.artifact_view().selected_native_groups(),
            fixture.late_closure
        );
        assert!(!restored
            .artifact_view()
            .selected_native_groups()
            .contains(&fixture.zero_edge));
        assert_eq!(
            restored.artifact_view().interface_dependencies().len(),
            1,
            "group-to-carrier custody is not an interface dependency"
        );
        let bytes = serde_json::to_vec(&baseline).unwrap();
        let decoded: RecoveryGraph = serde_json::from_slice(&bytes).unwrap();
        let decoded_node = decoded.node(Generation(1)).unwrap();
        assert_eq!(decoded_node.native_groups, node.native_groups);
        let recovered = decoded
            .capture_inventory(root.path())
            .unwrap()
            .context_with_roles(
                &decoded_node.artifact_refs,
                &decoded_node.native_groups,
                &decoded_node.compiler_roles,
                decoded_node.lexical.clone(),
            )
            .unwrap();
        assert_eq!(
            recovered.artifact_view().selected_native_groups(),
            fixture.late_closure
        );

        let mut extended = wire.clone();
        extended.nodes[0].native_groups.push(fixture.zero_edge);
        extended.seal().unwrap();
        let extended = snapshot(&extended);
        let added = extended.node(Generation(1)).unwrap();
        let extended_context = inventory
            .context_with_roles(
                &added.artifact_refs,
                &added.native_groups,
                &added.compiler_roles,
                added.lexical.clone(),
            )
            .unwrap();
        let mut expected = fixture.late_closure.clone();
        expected.insert(fixture.zero_edge);
        assert_eq!(
            extended_context.artifact_view().selected_native_groups(),
            expected
        );
        assert_eq!(
            baseline.artifacts().collect::<Vec<_>>(),
            extended.artifacts().collect::<Vec<_>>(),
            "registering one more existing group stores no new product bytes"
        );
        assert_eq!(
            restored.artifact_view().interface_dependencies(),
            extended_context.artifact_view().interface_dependencies()
        );
        assert_ne!(
            restored.semantic_sha256(),
            extended_context.semantic_sha256(),
            "zero-edge group selection is independently bound in context identity"
        );
        assert_ne!(baseline.checksum(), extended.checksum());
        let mut private_wire = extended.wire_for_test();
        private_wire.public_surfaces.clear();
        private_wire.nodes.retain(|node| node.id == Generation(1));
        private_wire.seal().unwrap();
        let manifest = root.path().join("declarations.json");
        assert!(matches!(
            stage_v2(&manifest, root.path(), snapshot(&private_wire))
                .unwrap()
                .publish(),
            RecoveryPublishOutcome::Durable { .. }
        ));
        let mut session = crate::session::SessionLib::open(
            crate::session::SessionId(41),
            root.path().join("session"),
            crate::session::ModuleEnv::standalone_default(),
        )
        .unwrap();
        session.attach_recovery_graph_v2(&manifest).unwrap();
        assert!(
            session.current_declarations().is_empty(),
            "private custody stays private"
        );
        session.reserve_join_generation_durable().unwrap();
        let reopened = read_v2(&manifest, root.path()).unwrap().unwrap();
        assert_eq!(
            reopened.graph.node(Generation(1)).unwrap().native_groups,
            expected.into_iter().collect::<Vec<_>>(),
            "later publication retains partial and zero-edge group custody"
        );
    }

    #[test]
    fn v8_native_group_selection_refuses_missing_altered_and_unauthenticated_facts() {
        use tidepool_toolchain::artifact_inventory::NativeGroupKey;
        let root = tempfile::tempdir().unwrap();
        let (wire, fixture) = compiled_group_selection(root.path());
        let baseline = snapshot(&wire);
        let inventory = baseline.capture_inventory(root.path()).unwrap();
        let node = baseline.node(Generation(1)).unwrap();
        assert!(
            inventory
                .context_with_roles(
                    &node.artifact_refs,
                    &node.native_groups,
                    &node.compiler_roles,
                    node.lexical.clone()
                )
                .is_ok(),
            "real positive before every refusal"
        );
        let missing = node
            .native_groups
            .iter()
            .copied()
            .filter(|key| key == &fixture.late)
            .collect::<Vec<_>>();
        assert!(inventory
            .context_with_roles(
                &node.artifact_refs,
                &missing,
                &node.compiler_roles,
                node.lexical.clone()
            )
            .is_err());
        let mut unknown = node.native_groups.clone();
        unknown.push(NativeGroupKey {
            artifact: fixture.late.artifact,
            original_ordinal: u32::MAX,
        });
        assert!(inventory
            .context_with_roles(
                &node.artifact_refs,
                &unknown,
                &node.compiler_roles,
                node.lexical.clone()
            )
            .is_err());
        let mut duplicate = node.native_groups.clone();
        duplicate.push(fixture.late);
        assert!(inventory
            .context_with_roles(
                &node.artifact_refs,
                &duplicate,
                &node.compiler_roles,
                node.lexical.clone()
            )
            .is_err());
        let mut outside = node.native_groups.clone();
        outside.push(NativeGroupKey {
            artifact: ArtifactId([0; 32]),
            original_ordinal: 0,
        });
        assert!(inventory
            .context_with_roles(
                &node.artifact_refs,
                &outside,
                &node.compiler_roles,
                node.lexical.clone()
            )
            .is_err());
        let mut altered = wire.clone();
        altered.nodes[0].native_groups.push(fixture.zero_edge);
        assert!(
            RecoveryGraph::from_wire(altered).is_err(),
            "checksum binds exact selection"
        );
        let manifest = root.path().join("declarations.json");
        let mut missing_field = serde_json::to_value(&baseline).unwrap();
        missing_field["nodes"][0]
            .as_object_mut()
            .unwrap()
            .remove("native_groups");
        let bytes = serde_json::to_vec(&missing_field).unwrap();
        fs::write(&manifest, &bytes).unwrap();
        assert!(
            read_v2(&manifest, root.path()).is_err(),
            "V8 never infers AllGroups"
        );
        assert_eq!(fs::read(&manifest).unwrap(), bytes);
        let mut old = serde_json::to_value(&baseline).unwrap();
        old["version"] = serde_json::json!(VERSION - 1);
        let bytes = serde_json::to_vec(&old).unwrap();
        fs::write(&manifest, &bytes).unwrap();
        assert_eq!(
            read_v2(&manifest, root.path()).err().unwrap().kind,
            RecoveryErrorKind::Format(RecoveryRefusal::UnsupportedOldFormat {
                version: u64::from(VERSION - 1)
            })
        );
        assert_eq!(fs::read(&manifest).unwrap(), bytes);
        // Selection omits an isolated body, but never weakens full-byte product
        // authentication. Corruption anywhere in that full product still refuses.
        let RecoveryArtifactClosure::Home(home) = home_artifact(&wire) else {
            unreachable!()
        };
        fs::write(
            root.path().join(&home.product_path),
            b"corrupt unused original body",
        )
        .unwrap();
        assert!(baseline.capture_inventory(root.path()).is_err());
    }

    #[test]
    fn v8_compiler_roles_are_required_exact_and_checksummed() {
        use tidepool_toolchain::artifact_inventory::CompilerInputRole;
        let wire = fixture();
        let original = wire.nodes[0].compiler_roles[0].original().unwrap();
        let interface = wire.nodes[0].compiler_roles[0].interface();
        let mut changed = wire.clone();
        changed.nodes[0].compiler_roles[0] = CompilerInputRole::InterfaceOnly { interface };
        assert!(
            RecoveryGraph::from_wire(changed.clone()).is_err(),
            "checksum binds roles independently of custody"
        );
        changed.seal().unwrap();
        assert_eq!(changed.nodes[0].artifact_refs, wire.nodes[0].artifact_refs);
        let mut duplicate = wire.clone();
        let repeated = duplicate.nodes[0].compiler_roles[0].clone();
        duplicate.nodes[0].compiler_roles.push(repeated);
        assert!(
            duplicate.seal().is_err(),
            "overlapping roles cannot be normalized away"
        );
        let mut wrong_kind = wire.clone();
        wrong_kind.nodes[0].compiler_roles[0] = CompilerInputRole::InterfaceOnly {
            interface: original,
        };
        assert!(wrong_kind.seal().is_err());
        let root = tempfile::tempdir().unwrap();
        let manifest = root.path().join("declarations.json");
        let mut missing = serde_json::to_value(&snapshot(&wire)).unwrap();
        missing["nodes"][0]
            .as_object_mut()
            .unwrap()
            .remove("compiler_roles");
        let bytes = serde_json::to_vec(&missing).unwrap();
        fs::write(&manifest, &bytes).unwrap();
        assert!(
            read_v2(&manifest, root.path()).is_err(),
            "v8 never issues reuse from retained availability"
        );
        assert_eq!(fs::read(&manifest).unwrap(), bytes);
    }

    fn graph_with_home(materialized: RecoveryArtifactRef) -> RecoveryGraphWire {
        let artifact = RecoveryArtifactClosure::Home(materialized);
        let key = artifact.artifact_id();
        let export = RecoveryExport {
            identity: identity("answer"),
            kind: RecoveryExportKind::Value,
            children: vec![],
        };
        let mut graph = RecoveryGraphWire {
            version: VERSION,
            public_schema: PAIRED_PUBLIC_SCHEMA.into(),
            source_session: 41,
            lineage: 99,
            high_water: Generation(2),
            public_surfaces: vec![RecoveryPublicSurface {
                owner: owner("root"),
                declaration_root: Some(Generation(2)),
                epoch: 0,
                bindings: vec![],
                source_instances: vec![],
                compiler_context: RecoveryCompilerContext::default(),
            }],
            nodes: vec![
                RecoveryNode {
                    id: Generation(1),
                    parent: None,
                    kind: RecoveryNodeKind::Authored,
                    implementation_refs: vec![],
                    lexical_roots: vec![],
                    lexical: vec![],
                    artifact_refs: vec![key],
                    native_groups: vec![],
                    compiler_roles: vec![],
                    exports: vec![export.clone()],
                    retracts: vec![],
                    workbench_imports: vec!["qualified Data.Map.Strict as Map".into()],
                    instances: RecoveryInstanceInventory::default(),
                    live_dependencies: vec![],
                    state: RecoveryNodeState::ExactArtifactClosure,
                },
                RecoveryNode {
                    id: Generation(2),
                    parent: Some(Generation(1)),
                    kind: RecoveryNodeKind::Join,
                    implementation_refs: vec![Generation(1)],
                    lexical_roots: vec![],
                    lexical: vec![],
                    artifact_refs: vec![],
                    native_groups: vec![],
                    compiler_roles: vec![],
                    exports: vec![export],
                    retracts: vec![],
                    workbench_imports: vec!["Data.Proxy (Proxy (..))".into()],
                    instances: RecoveryInstanceInventory::default(),
                    live_dependencies: vec![],
                    state: RecoveryNodeState::MissingArtifactClosure {
                        reason: "winner artifact unavailable".into(),
                    },
                },
            ],
            artifacts: vec![artifact],
            artifact_dependencies: vec![],
            checksum: String::new(),
        };
        install_fixture_canonical_interfaces(&mut graph);
        graph.seal().unwrap();
        graph
    }

    #[test]
    fn generation_zero_compiler_selection_roundtrips_and_history_stays_immutable() {
        let root = tempfile::tempdir().unwrap();
        let manifest = root.path().join("declarations.json");
        let later = compiled_fixture(root.path());
        let inventory = snapshot(&later).capture_inventory(root.path()).unwrap();
        let node = &later.nodes[0];
        let context = inventory
            .context_with_roles(
                &node.artifact_refs,
                &node.native_groups,
                &node.compiler_roles,
                vec![ExactLexicalNode {
                    owner: module("main", "Lib"),
                    imports: vec![],
                }],
            )
            .unwrap();
        let selection = RecoveryCompilerContext::capture(&context);
        assert!(!selection.artifact_refs.is_empty());
        assert!(!selection.native_groups.is_empty());
        assert!(!selection.compiler_roles.is_empty());
        let mut initial = later.clone();
        initial.nodes.clear();
        initial.high_water = Generation(0);
        initial.public_surfaces[0].declaration_root = None;
        initial.public_surfaces[0].compiler_context = selection.clone();
        initial.seal().unwrap();
        let captured = snapshot(&initial);
        let staged = stage_public_visibility_v2(
            &manifest,
            root.path(),
            &captured,
            owner("root"),
            0,
            vec![],
            vec![],
            None,
            selection.clone(),
        )
        .unwrap();
        let RecoveryPublishOutcome::Durable {
            graph: published, ..
        } = staged.publish()
        else {
            panic!("Generation0 compiler selection is durable")
        };
        let bytes = fs::read(&manifest).unwrap();
        let restored = read_v2_bytes(
            &manifest,
            root.path(),
            &bytes,
            RecoveryReadPurpose::Hydration,
        )
        .unwrap()
        .unwrap();
        assert!(restored.artifact_losses.is_empty());
        assert_eq!(restored.graph.high_water(), Generation(0));
        let surface = restored.graph.surface(&owner("root")).unwrap();
        assert_eq!(surface.declaration_root, None);
        let recovered = surface
            .compiler_context
            .restore(restored.inventory.as_ref().unwrap())
            .unwrap();
        assert_eq!(RecoveryCompilerContext::capture(&recovered), selection);
        let mut successor = published.candidate();
        successor.set_high_water(later.high_water).unwrap();
        for node in later.nodes {
            successor.insert_node(node).unwrap();
        }
        let mut surface = published.surface(&owner("root")).unwrap().clone();
        surface.declaration_root = Some(Generation(2));
        surface.epoch += 1;
        successor.replace_surface(surface);
        let successor = successor.seal().unwrap();
        assert_eq!(
            successor.surface(&owner("root")).unwrap().compiler_context,
            selection
        );
        assert_eq!(captured.high_water(), Generation(0));
        assert_eq!(captured.surface(&owner("root")).unwrap().epoch, 0);
        assert_eq!(
            published.surface(&owner("root")).unwrap().declaration_root,
            None
        );
        assert_eq!(
            published.surface(&owner("root")).unwrap().compiler_context,
            selection
        );
    }

    #[test]
    fn generation_zero_selected_artifact_loss_refuses_hydration() {
        for missing in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let manifest = root.path().join("declarations.json");
            let mut wire = compiled_fixture(root.path());
            let node = &wire.nodes[0];
            wire.public_surfaces[0].compiler_context = RecoveryCompilerContext {
                artifact_refs: node.artifact_refs.clone(),
                native_groups: node.native_groups.clone(),
                compiler_roles: node.compiler_roles.clone(),
                lexical: vec![],
            };
            wire.public_surfaces[0].declaration_root = None;
            wire.nodes.clear();
            wire.high_water = Generation(0);
            wire.seal().unwrap();
            let graph = snapshot(&wire);
            let original = match home_artifact(&wire) {
                RecoveryArtifactClosure::Home(home) => home.product_path.clone(),
                _ => unreachable!(),
            };
            let bytes = serde_json::to_vec(&graph).unwrap();
            fs::write(&manifest, &bytes).unwrap();
            let control = read_v2_bytes(
                &manifest,
                root.path(),
                &bytes,
                RecoveryReadPurpose::Hydration,
            )
            .unwrap()
            .unwrap();
            assert!(control.inventory.is_some());
            if missing {
                fs::remove_file(root.path().join(original)).unwrap();
            } else {
                fs::write(root.path().join(original), b"changed selected original").unwrap();
            }
            let refused = read_v2_bytes(
                &manifest,
                root.path(),
                &bytes,
                RecoveryReadPurpose::Hydration,
            )
            .unwrap()
            .unwrap();
            assert!(refused.inventory.is_none());
            assert!(!refused.artifact_losses.is_empty());
            assert_eq!(fs::read(&manifest).unwrap(), bytes);
        }
    }

    #[test]
    fn public_compiler_selection_refuses_unrepresented_roles_groups_and_lexical_owners() {
        let baseline = fixture();
        for corruption in 0..4 {
            let mut wire = baseline.clone();
            let selection = &mut wire.public_surfaces[0].compiler_context;
            selection.artifact_refs = wire.nodes[0].artifact_refs.clone();
            selection.compiler_roles = wire.nodes[0].compiler_roles.clone();
            match corruption {
                0 => selection.artifact_refs.push(ArtifactId([0xfe; 32])),
                1 => selection.native_groups.push(
                    tidepool_toolchain::artifact_inventory::NativeGroupKey {
                        artifact: ArtifactId([0xfd; 32]),
                        original_ordinal: 17,
                    },
                ),
                2 => selection.lexical.push(ExactLexicalNode {
                    owner: module("main", "Unselected"),
                    imports: vec![],
                }),
                3 => selection
                    .compiler_roles
                    .push(selection.compiler_roles[0].clone()),
                _ => unreachable!(),
            }
            assert!(
                wire.seal().is_err(),
                "invalid public selection {corruption}"
            );
        }
    }

    #[test]
    fn v9_requires_public_compiler_selection_and_refuses_v8_without_rewriting() {
        let root = tempfile::tempdir().unwrap();
        let manifest = root.path().join("declarations.json");
        for old_version in [false, true] {
            let mut value = serde_json::to_value(&snapshot(&fixture())).unwrap();
            if old_version {
                value["version"] = serde_json::json!(8);
                value["public_schema"] = serde_json::json!("paired-public-v8");
            } else {
                value["public_surfaces"][0]
                    .as_object_mut()
                    .unwrap()
                    .remove("compiler_context");
            }
            let bytes = serde_json::to_vec(&value).unwrap();
            fs::write(&manifest, &bytes).unwrap();
            let refusal = read_v2(&manifest, root.path()).err().unwrap();
            if old_version {
                assert_eq!(
                    refusal.kind,
                    RecoveryErrorKind::Format(RecoveryRefusal::UnsupportedOldFormat { version: 8 })
                );
            } else {
                assert!(refusal.detail.contains("compiler_context"));
            }
            assert_eq!(fs::read(&manifest).unwrap(), bytes);
        }
    }

    fn value_interface_from(home: &RecoveryArtifactRef, module: &str) -> RecoveryValueInterfaceRef {
        let mut value = RecoveryValueInterfaceRef {
            artifact_id: ArtifactId([0; 32]),
            interface: RecoveryJoinRef {
                toolchain_identity_sha256: home.toolchain_identity_sha256,
                unit: home.unit.clone(),
                module: module.into(),
                skinny_iface_sha256: home.skinny_iface_sha256,
                interface_path: home.interface_path.clone(),
                package_imports_path: home.package_imports_path.clone(),
                package_imports_sha256: home.package_imports_sha256,
            },
            requirements: Vec::new(),
        };
        value.artifact_id = ArtifactDescriptor::from_recovery_value_interface(&value).id;
        value
    }

    #[test]
    fn structural_graph_records_do_not_grant_recovery_authority() {
        let dir = tempfile::tempdir().unwrap();
        let graph = snapshot(&fixture());
        graph.validate().unwrap();
        assert!(!graph
            .validate_artifact_files(dir.path())
            .unwrap()
            .is_empty());
        assert!(graph.capture_inventory(dir.path()).is_err());
        let manifest = dir.path().join("declarations.json");
        assert!(stage_v2(&manifest, dir.path(), graph).is_err());
        assert!(!manifest.exists());
    }

    #[test]
    fn persistent_snapshots_share_history_payloads_across_publication_candidates() {
        let mut wire = fixture();
        let RecoveryArtifactClosure::Home(home) = home_artifact(&wire) else {
            unreachable!()
        };
        let mut value = value_interface_from(home, "Val1");
        value.requirements.push(module("main", "Lib"));
        value.artifact_id = ArtifactDescriptor::from_recovery_value_interface(&value).id;
        let canonical_id = ArtifactDescriptor::from_recovery_module_interface(
            home.module_interface.as_ref().unwrap(),
        )
        .id;
        let edge = RecoveryArtifactDependency {
            source: value.artifact_id,
            target: canonical_id,
            dependency: ArtifactDependency::Interface,
        };
        wire.nodes[0].artifact_refs.push(value.artifact_id);
        wire.artifacts
            .push(RecoveryArtifactClosure::ValueInterface(value));
        wire.artifact_dependencies.push(edge);
        let mut untouched_surface = wire.public_surfaces[0].clone();
        untouched_surface.owner = owner("untouched");
        untouched_surface.declaration_root = None;
        wire.public_surfaces.push(untouched_surface);
        let template = wire.nodes[0].clone();
        wire.nodes = (1..=128)
            .map(|id| RecoveryNode {
                id: Generation(id),
                ..template.clone()
            })
            .collect();
        wire.public_surfaces[0].declaration_root = None;
        wire.high_water = Generation(128);
        wire.seal().unwrap();
        let original = snapshot(&wire);
        let baseline = original.clone();
        let mut candidate = baseline.candidate();
        candidate.set_high_water(Generation(129)).unwrap();
        candidate
            .insert_node(RecoveryNode {
                id: Generation(129),
                ..template
            })
            .unwrap();
        let mut surface = baseline.surface(&owner("root")).unwrap().clone();
        surface.epoch = 1;
        candidate.replace_surface(surface);
        // Re-presenting immutable facts must not replace retained payloads.
        candidate
            .insert_artifact(baseline.artifacts().next().unwrap().clone())
            .unwrap();
        let successor = candidate.seal().unwrap();
        let unchanged = original
            .nodes()
            .filter(|node| std::ptr::eq(*node, successor.node(node.id).unwrap()))
            .count();
        assert_eq!(unchanged, 128);
        assert!(original
            .artifacts()
            .zip(successor.artifacts())
            .all(|(a, b)| std::ptr::eq(a, b)));
        assert!(original
            .nodes()
            .zip(baseline.nodes())
            .all(|(a, b)| std::ptr::eq(a, b)));
        assert!(original
            .artifact_dependencies()
            .zip(successor.artifact_dependencies())
            .all(|(a, b)| std::ptr::eq(a, b)));
        assert!(std::ptr::eq(
            original.surface(&owner("untouched")).unwrap(),
            successor.surface(&owner("untouched")).unwrap()
        ));
        assert!(!std::ptr::eq(
            original.surface(&owner("root")).unwrap(),
            successor.surface(&owner("root")).unwrap()
        ));
        assert_eq!(original.high_water(), Generation(128));
        assert_eq!(original.surface(&owner("root")).unwrap().epoch, 0);
        assert_eq!(successor.high_water(), Generation(129));
        assert_eq!(successor.nodes().count(), 129);
        let bytes = serde_json::to_vec(&successor).unwrap();
        let decoded: RecoveryGraph = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(decoded, successor);
        eprintln!(
            "persistent-recovery history_rows=128 retained_payload_copies=0 snapshot_and_candidate_roots_shared=true"
        );
    }

    #[test]
    fn wire_admission_preserves_original_revision_and_refuses_duplicate_rows() {
        let mut wire = fixture();
        wire.nodes.reverse();
        wire.checksum = checksum(&wire).unwrap();
        let raw_token = wire.checksum.clone();
        let admitted = snapshot(&wire);
        assert_eq!(admitted.checksum(), raw_token);
        assert!(serde_json::to_vec(&admitted).is_err());
        assert_eq!(
            admitted.nodes().map(|node| node.id).collect::<Vec<_>>(),
            vec![Generation(1), Generation(2)]
        );
        let mut candidate = admitted.candidate();
        candidate.set_high_water(Generation(3)).unwrap();
        let canonical = candidate.seal().unwrap();
        let encoded = serde_json::to_vec(&canonical).unwrap();
        let decoded: RecoveryGraph = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(decoded, canonical);
        assert_eq!(admitted.checksum(), raw_token);
        let canonical_wire = canonical.wire_for_test();
        assert_eq!(encoded, serde_json::to_vec(&canonical_wire).unwrap());
        for kind in 0..4 {
            let mut duplicate = wire.clone();
            match kind {
                0 => duplicate.nodes.push(duplicate.nodes[0].clone()),
                1 => duplicate.artifacts.push(duplicate.artifacts[0].clone()),
                2 => duplicate
                    .public_surfaces
                    .push(duplicate.public_surfaces[0].clone()),
                _ => {
                    let edge = RecoveryArtifactDependency {
                        source: duplicate.artifacts[0].artifact_id(),
                        target: duplicate.artifacts[0].artifact_id(),
                        dependency: ArtifactDependency::Interface,
                    };
                    duplicate.artifact_dependencies = vec![edge.clone(), edge];
                }
            }
            duplicate.checksum = checksum(&duplicate).unwrap();
            assert!(RecoveryGraph::from_wire(duplicate).is_err());
        }
    }

    #[test]
    fn noncanonical_value_requirements_preserve_graph_admission_and_candidates() {
        let mut wire = fixture();
        let RecoveryArtifactClosure::Home(home) = home_artifact(&wire) else {
            unreachable!()
        };
        let other = value_interface_from(home, "Other");
        let mut value = value_interface_from(home, "Val1");
        value.requirements = vec![module("main", "Other"), module("main", "Lib")];
        value.artifact_id = ArtifactDescriptor::from_recovery_value_interface(&value).id;
        let home_id = home_artifact(&wire).artifact_id();
        let RecoveryArtifactClosure::Home(home) = home_artifact(&wire) else {
            unreachable!()
        };
        let canonical_id = ArtifactDescriptor::from_recovery_module_interface(
            home.module_interface.as_ref().unwrap(),
        )
        .id;
        for node in &mut wire.nodes {
            node.artifact_refs
                .extend([home_id, other.artifact_id, value.artifact_id]);
        }
        wire.artifact_dependencies.extend([
            RecoveryArtifactDependency {
                source: value.artifact_id,
                target: canonical_id,
                dependency: ArtifactDependency::Interface,
            },
            RecoveryArtifactDependency {
                source: value.artifact_id,
                target: other.artifact_id,
                dependency: ArtifactDependency::Interface,
            },
        ]);
        wire.artifacts.extend([
            RecoveryArtifactClosure::ValueInterface(other),
            RecoveryArtifactClosure::ValueInterface(value.clone()),
        ]);
        install_fixture_canonical_interfaces(&mut wire);
        wire.seal().unwrap();
        let mut wrong_target = wire.clone();
        wrong_target
            .artifact_dependencies
            .iter_mut()
            .find(|edge| edge.source == value.artifact_id && edge.target == canonical_id)
            .unwrap()
            .target = home_id;
        assert_eq!(
            wrong_target.seal().unwrap_err().detail,
            "recovery node 1 has a value interface requirement without its direct artifact edge"
        );
        let mut native_id_orders = [false; 2];
        // These product digests place the fixture's native descriptor on each
        // side of its unchanged canonical interface descriptor.
        for (product_digest, native_after_canonical) in [(0, false), (1, true)] {
            let RecoveryArtifactClosure::Home(mut native) = home_artifact(&wire).clone() else {
                unreachable!()
            };
            native.product_sha256 = [product_digest; 32];
            let native = RecoveryArtifactClosure::Home(native);
            let native_id = native.artifact_id();
            assert_ne!(native_id, canonical_id);
            assert_eq!(native_id > canonical_id, native_after_canonical);
            let order = usize::from(native_after_canonical);
            let mut ordered = wire.clone();
            *home_artifact_mut(&mut ordered) = native;
            for node in &mut ordered.nodes {
                for id in &mut node.artifact_refs {
                    if *id == home_id {
                        *id = native_id;
                    }
                }
                for role in &mut node.compiler_roles {
                    if let tidepool_toolchain::artifact_inventory::CompilerInputRole::ReusableOriginal {
                        original, ..
                    } = role {
                        if *original == home_id {
                            *original = native_id;
                        }
                    }
                }
            }
            for edge in &mut ordered.artifact_dependencies {
                if edge.source == home_id {
                    edge.source = native_id;
                }
            }
            ordered.seal().unwrap();
            snapshot(&ordered).validate().unwrap();
            native_id_orders[order] = true;
        }
        assert_eq!(
            native_id_orders, [true; 2],
            "owning native variants must cover both sides of the canonical content ID"
        );
        let stored = wire
            .artifacts
            .iter_mut()
            .find_map(|artifact| match artifact {
                RecoveryArtifactClosure::ValueInterface(reference)
                    if reference.artifact_id == value.artifact_id =>
                {
                    Some(reference)
                }
                _ => None,
            })
            .unwrap();
        stored.requirements.reverse();
        wire.checksum = checksum(&wire).unwrap();
        let original_token = wire.checksum.clone();
        let original_bytes = serde_json::to_vec(&wire).unwrap();
        let recovered =
            RecoveryGraph::from_wire(serde_json::from_slice(&original_bytes).unwrap()).unwrap();
        assert_eq!(recovered.checksum(), original_token);
        assert!(serde_json::to_vec(&recovered).is_err());
        let mut candidate = recovered.candidate();
        candidate
            .insert_artifact(RecoveryArtifactClosure::ValueInterface(value.clone()))
            .unwrap();
        let mut changed = value.clone();
        changed.requirements.pop();
        assert!(candidate
            .insert_artifact(RecoveryArtifactClosure::ValueInterface(changed))
            .is_err());
        let successor = candidate.seal().unwrap();
        assert_eq!(recovered.checksum(), original_token);
        let reference = successor
            .artifacts()
            .find_map(|artifact| match artifact {
                RecoveryArtifactClosure::ValueInterface(reference)
                    if reference.artifact_id == value.artifact_id =>
                {
                    Some(reference)
                }
                _ => None,
            })
            .unwrap();
        let mut expected = value.requirements;
        expected.sort();
        assert_eq!(reference.requirements, expected);
        let mut duplicate = wire;
        let reference = duplicate
            .artifacts
            .iter_mut()
            .find_map(|artifact| match artifact {
                RecoveryArtifactClosure::ValueInterface(reference)
                    if reference.artifact_id == value.artifact_id =>
                {
                    Some(reference)
                }
                _ => None,
            })
            .unwrap();
        reference
            .requirements
            .push(reference.requirements[0].clone());
        duplicate.checksum = checksum(&duplicate).unwrap();
        assert!(RecoveryGraph::from_wire(duplicate).is_err());
    }

    #[test]
    fn completed_source_refusal_aborts_recovery_without_tombstoning_the_original() {
        let mut wire = fixture();
        wire.public_surfaces[0].declaration_root = Some(Generation(1));
        wire.seal().unwrap();
        let graph = snapshot(&wire);
        let input = PathBuf::from("/retained/source.hs");
        let cause = tidepool_toolchain::cache::DependencyEvidenceFailure::Resolution {
            index: 85,
            reason:
                tidepool_toolchain::cache::ResolutionWitnessFailure::NegativeCandidateUnavailable {
                    index: 16,
                },
        };
        let refusal = artifact_error_loss(
            home_artifact(&wire),
            RecoveryArtifactError::CompletedSourceEvidence {
                input: input.clone(),
                failure: Box::new(cause.clone()),
            },
        )
        .unwrap_err();
        assert_eq!(
            refusal.kind,
            RecoveryErrorKind::SourceEvidence {
                input: input.clone(),
                failure: cause.clone()
            }
        );
        assert!(
            matches!(super::super::graph_error(Path::new("declarations.json"), refusal),
            crate::session::SessionError::Compile(crate::CompileError::CompilerEvidence(error))
                if matches!(error.as_ref(), tidepool_toolchain::certified_products::CertificationError::CompletedSourceEvidence {
                    input: actual_input, failure: actual_cause,
                } if actual_input == &input && actual_cause.as_ref() == &cause))
        );
        assert!(matches!(
            graph.projection(&owner("root"), &BTreeMap::new()).unwrap()[&identity("answer")],
            RecoveryHead::Available { .. }
        ));
    }

    #[test]
    fn admission_accounting_refuses_recovery_without_tombstoning_the_original() {
        use tidepool_repr::execution_schema::{InventoryDecodeLimits, InventoryOperation};
        let mut wire = fixture();
        wire.public_surfaces[0].declaration_root = Some(Generation(1));
        wire.seal().unwrap();
        let graph = snapshot(&wire);
        let operation = InventoryOperation::new(InventoryDecodeLimits {
            max_work: 0,
            ..InventoryDecodeLimits::default()
        });
        let cause = tidepool_toolchain::recovery_artifacts::RecoveryAdmissionFailure::Decode(
            operation.decode_value(&[0x80], 1).unwrap_err(),
        );
        let failure = artifact_error_loss(
            home_artifact(&wire),
            RecoveryArtifactError::InventoryAccounting(cause.clone()),
        )
        .unwrap_err();
        assert_eq!(
            failure.kind,
            RecoveryErrorKind::InventoryAccounting(cause.clone())
        );
        assert!(matches!(
            crate::session::recovery::graph_error(Path::new("declarations.json"), failure),
            crate::session::SessionError::RecoveryInventoryRefused { cause: actual, .. }
                if actual == cause
        ));
        assert!(matches!(
            graph.projection(&owner("root"), &BTreeMap::new()).unwrap()[&identity("answer")],
            RecoveryHead::Available { .. }
        ));
    }

    #[test]
    fn producer_mismatch_tombstones_the_original_without_resurrection() {
        let mut wire = fixture();
        wire.public_surfaces[0].declaration_root = Some(Generation(1));
        wire.seal().unwrap();
        let graph = snapshot(&wire);
        let artifact = home_artifact(&wire);
        assert!(matches!(
            graph.projection(&owner("root"), &BTreeMap::new()).unwrap()[&identity("answer")],
            RecoveryHead::Available { .. }
        ));
        let loss = artifact_error_loss(
            artifact,
            RecoveryArtifactError::ExecutionSourceProducerMismatch {
                unit: "main".into(),
                module: "Lib".into(),
                expected: [1; 32],
                actual: [2; 32],
            },
        )
        .unwrap();
        assert_eq!(loss.component, RecoveryArtifactComponent::Product);
        assert!(matches!(
            &loss.kind,
            RecoveryArtifactLossKind::ExecutionSourceProducerMismatch {
                expected,
                actual,
                ..
            } if *expected == [1; 32] && *actual == [2; 32]
        ));
        let losses = BTreeMap::from([(artifact.artifact_id(), vec![loss])]);
        assert!(matches!(
            graph.projection(&owner("root"), &losses).unwrap()[&identity("answer")],
            RecoveryHead::Tombstone(_)
        ));
    }

    #[test]
    fn interface_recovery_retains_native_requirements_and_selected_evidence() {
        let mut graph = fixture();
        let RecoveryArtifactClosure::Home(home) = home_artifact(&graph) else {
            unreachable!()
        };
        let value = value_interface_from(home, "Val1");
        let value_id = value.artifact_id;
        let original_id = home_artifact(&graph).artifact_id();
        let canonical_id = ArtifactDescriptor::from_recovery_module_interface(
            home.module_interface.as_ref().unwrap(),
        )
        .id;
        graph
            .artifacts
            .push(RecoveryArtifactClosure::ValueInterface(value));
        for node in &mut graph.nodes {
            node.artifact_refs = vec![original_id, canonical_id, value_id];
            node.state = RecoveryNodeState::LiveValueDependency {
                reason: "lost x".into(),
            };
            node.live_dependencies = vec![RecoveryLiveDependency::NativeBinding {
                artifact_id: value_id,
                binding: RecoverySourceIdentity {
                    unit: "main".into(),
                    module: "Val1".into(),
                    namespace: "value".into(),
                    occurrence: "x".into(),
                    record_parent: None,
                },
                generation: 1,
            }];
            node.instances.classes.push(RecoveryInstanceEvidence {
                dfun: identity("dfun"),
                class: identity("class"),
                selected: true,
                selected_axioms: vec![],
            });
        }
        graph.public_surfaces[0]
            .bindings
            .push(RecoveryPublicBinding {
                name: "x".into(),
                owner: RecoveryBindingId {
                    session: 41,
                    variable: 7,
                },
            });
        graph.public_surfaces[0].epoch = 1;
        graph.seal().unwrap();
        let before = graph.clone();
        assert!(matches!(
            snapshot(&graph)
                .projection(&owner("root"), &BTreeMap::new())
                .unwrap()[&identity("answer")],
            RecoveryHead::Tombstone(_)
        ));
        assert!(matches!(
            snapshot(&graph)
                .interface_projection(&owner("root"), &BTreeMap::new())
                .unwrap()[&identity("answer")],
            RecoveryHead::Available {
                winner: Generation(2),
                ..
            }
        ));
        assert_eq!(
            snapshot(&graph)
                .public_binding_tombstones(&owner("root"))
                .unwrap()[0]
                .winner
                .variable,
            7
        );
        assert_eq!(graph, before);

        let losses = BTreeMap::from([(
            original_id,
            vec![RecoveryArtifactLoss {
                component: RecoveryArtifactComponent::Product,
                path: PathBuf::from("missing.product"),
                kind: RecoveryArtifactLossKind::Missing,
            }],
        )]);
        assert!(matches!(
            snapshot(&graph)
                .interface_projection(&owner("root"), &losses)
                .unwrap()[&identity("answer")],
            RecoveryHead::Tombstone(_)
        ));
        graph.nodes[1].state = RecoveryNodeState::MissingArtifactClosure {
            reason: "missing closure".into(),
        };
        graph.nodes[1].live_dependencies.clear();
        graph.seal().unwrap();
        assert!(matches!(
            snapshot(&graph)
                .interface_projection(&owner("root"), &BTreeMap::new())
                .unwrap()[&identity("answer")],
            RecoveryHead::Tombstone(_)
        ));
    }

    #[test]
    fn private_only_exact_nodes_survive_restart_without_lexical_visibility() {
        use crate::session::{ModuleEnv, SessionId, SessionLib};
        use tidepool_codegen::scope::ScopeId;

        let root = tempfile::tempdir().unwrap();
        let manifest = root.path().join("declarations.json");
        let mut graph = compiled_fixture(root.path());
        graph.public_surfaces.clear();
        graph.nodes.retain(|node| node.id == Generation(1));
        graph.seal().unwrap();
        assert!(matches!(
            stage_v2(&manifest, root.path(), snapshot(&graph))
                .unwrap()
                .publish(),
            RecoveryPublishOutcome::Durable { .. }
        ));

        let mut reopened = SessionLib::open(
            SessionId(41),
            root.path().join("session"),
            ModuleEnv::standalone_default(),
        )
        .unwrap();
        reopened.attach_recovery_graph_v2(&manifest).unwrap();
        assert_eq!(reopened.generation(), Generation(2));
        assert_eq!(reopened.scope_tip(ScopeId::ROOT), Generation(0));
        assert!(reopened.current_module().is_none());
        assert!(reopened.current_declarations().is_empty());
        assert_eq!(
            reopened.reserve_join_generation_durable().unwrap(),
            Generation(3)
        );
        let retained = read_v2(&manifest, root.path()).unwrap().unwrap();
        assert_eq!(
            retained
                .graph
                .nodes()
                .map(|node| node.id)
                .collect::<Vec<_>>(),
            graph.nodes.iter().map(|node| node.id).collect::<Vec<_>>()
        );
        assert_eq!(retained.graph.high_water(), Generation(3));
    }

    #[test]
    fn private_recovery_refuses_live_marker_not_in_verified_native_inventory() {
        use crate::session::{ModuleEnv, SessionId, SessionLib};
        let root = tempfile::tempdir().unwrap();
        let manifest = root.path().join("declarations.json");
        let mut graph = compiled_fixture(root.path());
        graph.public_surfaces.clear();
        graph.nodes.truncate(1);
        let native_id = home_artifact(&graph).artifact_id();
        let node = &mut graph.nodes[0];
        node.lexical_roots = vec![module("main", "Lib")];
        node.lexical = vec![ExactLexicalNode {
            owner: module("main", "Lib"),
            imports: vec![],
        }];
        node.state = RecoveryNodeState::LiveValueDependency {
            reason: "claimed native lease".into(),
        };
        node.live_dependencies
            .push(RecoveryLiveDependency::NativeBinding {
                artifact_id: native_id,
                binding: RecoverySourceIdentity {
                    unit: "main".into(),
                    module: "Lib".into(),
                    namespace: "value".into(),
                    occurrence: "answer".into(),
                    record_parent: None,
                },
                generation: 1,
            });
        graph.seal().unwrap();
        assert!(matches!(
            stage_v2(&manifest, root.path(), snapshot(&graph))
                .unwrap()
                .publish(),
            RecoveryPublishOutcome::Durable { .. }
        ));
        let bytes = fs::read(&manifest).unwrap();
        let mut reopened = SessionLib::open(
            SessionId(42),
            root.path().join("session"),
            ModuleEnv::standalone_default(),
        )
        .unwrap();
        assert!(matches!(reopened.attach_recovery_graph_v2(&manifest),
            Err(crate::session::SessionError::RecoveryManifest { detail, .. })
                if detail.contains("live dependencies differ from verified original native requirements")));
        assert_eq!(fs::read(&manifest).unwrap(), bytes);
        graph.nodes[0].live_dependencies = vec![RecoveryLiveDependency::Instance {
            dfun: identity("dfun"),
        }];
        assert!(graph
            .seal()
            .unwrap_err()
            .detail
            .contains("unsupported recovery instance"));
    }

    #[test]
    fn private_recovery_validates_authentic_native_markers_and_later_tamper() {
        use crate::session::turn::scaling_tests::{execute_cell, QuietOutput, ScalePublication};
        use crate::session::{
            ModuleEnv, PersistentSession, ResidentSession, SessionId, SessionLib,
        };
        use std::sync::Arc;
        use tidepool_codegen::{prepared_program::ImageRegistry, scope::ScopeId};
        use tidepool_testing::effect_surface::TestEffectSurface;

        struct RunOwner {
            root: PathBuf,
            _lock: fs::File,
        }
        impl crate::session::RecoveryRunAuthority for RunOwner {
            fn owns_run(&self, root: &Path) -> std::io::Result<bool> {
                Ok(root.canonicalize()? == self.root)
            }
        }
        tidepool_testing::eval_harness::require_extract();
        let root = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(root.path().join("run-owner.lock"))
            .unwrap();
        lock.try_lock().unwrap();
        let run_owner = Arc::new(RunOwner {
            root: root.path().canonicalize().unwrap(),
            _lock: lock,
        });
        let effects = TestEffectSurface::minimal(&[]).unwrap();
        let producer_manifest = root.path().join("producer.json");
        let mut lib = SessionLib::open(
            SessionId(41),
            source.path(),
            ModuleEnv::standalone_default(),
        )
        .unwrap()
        .with_validation_include(effects.include_paths().to_vec());
        lib.attach_owned_recovery_graph_v3(&producer_manifest, run_owner.clone())
            .unwrap();
        let images = Arc::new(ImageRegistry::new());
        let mut persistent = PersistentSession::new(Some(lib), 1024 * 1024);
        persistent.set_image_registry(images.clone());
        let public = persistent.mint_scope(ScopeId::ROOT).unwrap();
        let mut producer =
            ResidentSession::from_persistent_for_test(frunk::HNil, QuietOutput, persistent);
        producer
            .initialize_durable_public_scope(owner("root"), public)
            .unwrap();
        let publication = ScalePublication::Durable {
            owner: owner("root"),
            manifest: producer_manifest.clone(),
        };
        for (label, cell, declarations) in [
            ("bound_x", include_str!("fixtures/recovery-live-bind.hs"), 0),
            (
                "captures_x",
                include_str!("fixtures/recovery-live-declaration.hs"),
                1,
            ),
        ] {
            execute_cell(
                &mut producer,
                public,
                &effects,
                &images,
                (0, 0),
                label,
                cell,
                declarations,
                &publication,
            );
        }
        let produced = read_v2(&producer_manifest, root.path()).unwrap().unwrap();
        assert!(produced.artifact_losses.is_empty());
        let mut graph = produced.graph.wire_for_test();
        let original_node = graph
            .nodes
            .iter()
            .find(|node| {
                node.kind == RecoveryNodeKind::Authored && !node.live_dependencies.is_empty()
            })
            .expect("compiler-issued authored declaration captures the live x")
            .clone();
        assert_eq!(original_node.live_dependencies.len(), 1);
        let exact = original_node.live_dependencies.first().unwrap().clone();
        let RecoveryLiveDependency::NativeBinding {
            generation: required_generation,
            binding,
            ..
        } = &exact
        else {
            panic!("compiler-issued x native binding requirement");
        };
        assert_eq!(binding.occurrence, "x");
        let required_generation = *required_generation;
        let original = graph
            .artifacts
            .iter()
            .find(|artifact| {
                matches!(artifact, RecoveryArtifactClosure::Home(home)
                if home.module == tidepool_repr::SessionModule::lib(original_node.id).module_name())
            })
            .expect("compiler-issued original native product")
            .clone();
        // The genuine published Join retains the same original native body.
        // Projecting its execution selection to empty preserves availability
        // and full-body lifetime requirements, rather than fabricating a proof.
        let mut join_graph = graph.clone();
        let join_index = join_graph
            .nodes
            .iter()
            .position(|node| {
                node.kind == RecoveryNodeKind::Join
                    && node
                        .exports
                        .iter()
                        .any(|export| export.identity.occurrence == "answer")
            })
            .expect("actual accepted published Join exports answer");
        let original_join = join_graph.nodes[join_index].clone();
        assert_eq!(original_join.live_dependencies, vec![exact.clone()]);
        assert!(!original_join.native_groups.is_empty());
        join_graph.nodes[join_index].native_groups.clear();
        assert_eq!(join_graph.artifacts, graph.artifacts);
        join_graph.seal().unwrap();
        let join_snapshot = snapshot(&join_graph);
        let join_inventory = join_snapshot.capture_inventory(root.path()).unwrap();
        let projected_join = &join_graph.nodes[join_index];
        assert_eq!(projected_join.artifact_refs, original_join.artifact_refs);
        assert_eq!(projected_join.exports, original_join.exports);
        assert_eq!(
            projected_join.live_dependencies,
            original_join.live_dependencies
        );
        let projected_context = join_inventory
            .context_with_roles(
                &projected_join.artifact_refs,
                &projected_join.native_groups,
                &projected_join.compiler_roles,
                projected_join.lexical.clone(),
            )
            .unwrap();
        assert!(projected_context
            .artifact_view()
            .selected_native_groups()
            .is_empty());
        let originals = projected_context
            .artifact_view()
            .descriptors()
            .into_iter()
            .filter(|descriptor| {
                descriptor.kind
                    == tidepool_toolchain::artifact_inventory::ArtifactKind::OriginalModule
            })
            .map(|descriptor| descriptor.id)
            .collect();
        let selected =
            crate::session::selected_native_roots(projected_context.artifact_view(), &originals);
        assert!(
            crate::session::certified_native_dependencies(&projected_context, &selected)
                .unwrap()
                .is_empty()
        );
        crate::session::recovery_hydration::validate_recovery_native_markers(
            projected_join,
            &projected_context,
        )
        .unwrap();
        let join_manifest = root.path().join("accepted-join.json");
        assert!(matches!(
            stage_v2(&join_manifest, root.path(), join_snapshot)
                .unwrap()
                .publish(),
            RecoveryPublishOutcome::Durable { .. }
        ));
        let join_bytes = fs::read(&join_manifest).unwrap();
        let reopened_join = read_v2(&join_manifest, root.path()).unwrap().unwrap();
        assert!(reopened_join.artifact_losses.is_empty());
        assert_eq!(
            reopened_join
                .graph
                .node(original_join.id)
                .unwrap()
                .live_dependencies,
            original_join.live_dependencies
        );
        let projection = reopened_join.projection(&owner("root")).unwrap();
        assert!(projection.values().any(|head| matches!(head,
            RecoveryHead::Tombstone(RecoveryTombstone { identity, reason: RecoveryLossReason::LiveValueDependency(_), .. })
                if identity.occurrence == "answer")));
        assert!(reopened_join
            .graph
            .public_binding_tombstones(&owner("root"))
            .unwrap()
            .iter()
            .any(|binding| binding.name == "x"));
        let mut recovered_lib = SessionLib::open(
            SessionId(43),
            root.path().join("accepted-join-session"),
            ModuleEnv::standalone_default(),
        )
        .unwrap()
        .with_validation_include(effects.include_paths().to_vec());
        recovered_lib
            .attach_owned_recovery_graph_v3(&join_manifest, run_owner.clone())
            .unwrap();
        let mut recovered = PersistentSession::new(Some(recovered_lib), 1024 * 1024);
        assert!(recovered.prepared().is_none());
        let recovered_scope = recovered.recover_public_scope(&owner("root")).unwrap();
        let recovered_visibility = recovered
            .public_visibility_snapshot_in(recovered_scope)
            .unwrap();
        assert!(recovered_visibility.bindings.is_empty());
        assert!(recovered_visibility.source_instances.is_empty());
        assert!(recovered.prepared().is_none());
        assert_eq!(fs::read(&join_manifest).unwrap(), join_bytes);

        // Retained node authority remains available for private recovery, but
        // an explicit empty public selection must not acquire that context.
        let empty_manifest = root.path().join("empty-public-selection.json");
        let mut empty_public = join_graph.clone();
        let surface = empty_public
            .public_surfaces
            .iter_mut()
            .find(|surface| surface.owner == owner("root"))
            .unwrap();
        assert!(surface.declaration_root.is_some());
        assert!(!surface.compiler_context.artifact_refs.is_empty());
        surface.compiler_context = RecoveryCompilerContext::default();
        empty_public.seal().unwrap();
        let empty_graph = snapshot(&empty_public);
        empty_graph.capture_inventory(root.path()).unwrap();
        assert!(matches!(
            stage_v2(&empty_manifest, root.path(), empty_graph)
                .unwrap()
                .publish(),
            RecoveryPublishOutcome::Durable { .. }
        ));
        let empty_bytes = fs::read(&empty_manifest).unwrap();
        let mut empty_lib = SessionLib::open(
            SessionId(44),
            root.path().join("empty-public-session"),
            ModuleEnv::standalone_default(),
        )
        .unwrap()
        .with_validation_include(effects.include_paths().to_vec());
        empty_lib
            .attach_owned_recovery_graph_v3(&empty_manifest, run_owner.clone())
            .unwrap();
        assert!(empty_lib.log.joined_context_at(original_join.id).is_some());
        assert!(empty_lib
            .recovered_public_compiler_context(&owner("root"))
            .unwrap()
            .is_none());
        let mut empty_session = PersistentSession::new(Some(empty_lib), 0);
        let scopes_before = empty_session.scope_tree().len();
        assert!(matches!(
            empty_session.recover_public_scope(&owner("root")),
            Err(crate::session::SessionError::RecoveryManifest { detail, .. })
                if detail.contains("lacks its exact public compiler selection")
        ));
        assert_eq!(empty_session.scope_tree().len(), scopes_before);
        assert!(empty_session.lib().durable_public_scopes.is_empty());
        struct UnexpectedSuccessorValidation;
        impl crate::session::RecoverySuccessorAuthority for UnexpectedSuccessorValidation {
            fn validate_successor(
                &self,
                _: &Path,
                _: &RecoveryPublicOwner,
                _: &RecoveryPublicOwner,
                _: SessionId,
                _: ScopeId,
            ) -> std::io::Result<bool> {
                panic!("absent public compiler selection refuses before actor authority");
            }
        }
        let target = empty_session.mint_isolated_scope();
        empty_session
            .seal_recovery_initialization_scope(target)
            .unwrap();
        let before_transfer = empty_session.public_visibility_snapshot_in(target).unwrap();
        let successor = RecoveryPublicOwner {
            incarnation: 2,
            ..owner("root")
        };
        assert!(matches!(
            empty_session.transfer_recovered_public_owner(
                &owner("root"), successor, target, Arc::new(UnexpectedSuccessorValidation),
            ),
            Err(crate::session::SessionError::RecoveryManifest { detail, .. })
                if detail.contains("lacks its exact public compiler selection")
        ));
        assert_eq!(
            empty_session.public_visibility_snapshot_in(target).unwrap(),
            before_transfer
        );
        assert!(empty_session.lib().durable_public_scopes.is_empty());
        assert_eq!(fs::read(&empty_manifest).unwrap(), empty_bytes);
        assert_eq!(fs::read(&join_manifest).unwrap(), join_bytes);
        let mut wrong_join_generation = exact.clone();
        let RecoveryLiveDependency::NativeBinding { generation, .. } = &mut wrong_join_generation
        else {
            panic!("genuine Join retains native binding requirement");
        };
        *generation += 1;
        let mut wrong_join_owner = exact.clone();
        let RecoveryLiveDependency::NativeBinding { binding, .. } = &mut wrong_join_owner else {
            panic!("genuine Join retains native binding requirement");
        };
        binding.module = original.owner().module;
        // Omission and generation substitution preserve the marker's exact
        // artifact identity. The native custody reader refuses both after seal.
        for (index, (markers, state)) in [
            (vec![], RecoveryNodeState::ExactArtifactClosure),
            (vec![wrong_join_generation], original_join.state.clone()),
        ]
        .into_iter()
        .enumerate()
        {
            let mut refused = join_graph.clone();
            refused.nodes[join_index].live_dependencies = markers;
            refused.nodes[join_index].state = state;
            refused.seal().unwrap();
            let path = root.path().join(format!("refused-join-{index}.json"));
            assert!(matches!(
                stage_v2(&path, root.path(), snapshot(&refused))
                    .unwrap()
                    .publish(),
                RecoveryPublishOutcome::Durable { .. }
            ));
            let bytes = fs::read(&path).unwrap();
            let mut lib = SessionLib::open(
                SessionId(50 + index as u64),
                root.path().join(format!("refused-join-session-{index}")),
                ModuleEnv::standalone_default(),
            )
            .unwrap()
            .with_validation_include(effects.include_paths().to_vec());
            assert!(
                matches!(lib.attach_owned_recovery_graph_v3(&path, run_owner.clone()),
                Err(crate::session::SessionError::RecoveryManifest { detail, .. })
                    if detail.contains("live dependencies differ from verified original native requirements"))
            );
            assert_eq!(lib.generation(), Generation(0));
            assert_eq!(fs::read(&path).unwrap(), bytes);
        }
        // Owner substitution contradicts the marker's authenticated artifact
        // identity, so the graph owner refuses it before a manifest is issued.
        let owner_manifest = root.path().join("refused-join-owner.json");
        let accepted_generation = recovered.lib().generation();
        let mut refused_owner = join_graph.clone();
        refused_owner.nodes[join_index].live_dependencies = vec![wrong_join_owner];
        let refusal = refused_owner.seal().unwrap_err();
        assert!(matches!(refusal.kind, RecoveryErrorKind::Manifest));
        assert_eq!(
            refusal.detail,
            "native binding dependency differs from its certified original identity"
        );
        assert!(refusal.path.is_none());
        assert!(!owner_manifest.exists());
        assert_eq!(fs::read(&join_manifest).unwrap(), join_bytes);
        assert_eq!(recovered.lib().generation(), accepted_generation);
        assert!(recovered.prepared().is_none());
        // Private hydration retains the original implementation and its exact
        // live requirements, without admitting a lexical public selector.
        graph.public_surfaces.clear();
        graph.nodes = vec![original_node];
        drop(producer.take_lib());
        drop(producer);
        graph.seal().unwrap();
        let manifest = root.path().join("accepted.json");
        assert!(matches!(
            stage_v2(&manifest, root.path(), snapshot(&graph))
                .unwrap()
                .publish(),
            RecoveryPublishOutcome::Durable { .. }
        ));
        let mut accepted = SessionLib::open(
            SessionId(41),
            root.path().join("accepted-session"),
            ModuleEnv::standalone_default(),
        )
        .unwrap();
        accepted
            .attach_owned_recovery_graph_v3(&manifest, run_owner.clone())
            .unwrap();
        assert!(!accepted
            .validate_recovered_public_owner(&owner("root"))
            .unwrap());
        assert_eq!(accepted.generation(), graph.high_water);
        assert!(accepted.current_module().is_none());

        let mut wrong_generation = exact.clone();
        let RecoveryLiveDependency::NativeBinding { generation, .. } = &mut wrong_generation else {
            unreachable!()
        };
        *generation += 1;
        let wrong_owner = RecoveryLiveDependency::NativeBinding {
            artifact_id: original.artifact_id(),
            binding: RecoverySourceIdentity {
                unit: original.owner().unit,
                module: original.owner().module,
                namespace: "value".into(),
                occurrence: "x".into(),
                record_parent: None,
            },
            generation: required_generation,
        };
        for (index, markers) in [vec![], vec![wrong_generation], vec![wrong_owner]]
            .into_iter()
            .enumerate()
        {
            let mut forged = graph.clone();
            forged.nodes[0].live_dependencies = markers;
            if forged.nodes[0].live_dependencies.is_empty() {
                forged.nodes[0].state = RecoveryNodeState::ExactArtifactClosure;
            }
            forged.seal().unwrap();
            let path = root.path().join(format!("refused-{index}.json"));
            assert!(matches!(
                stage_v2(&path, root.path(), snapshot(&forged))
                    .unwrap()
                    .publish(),
                RecoveryPublishOutcome::Durable { .. }
            ));
            let bytes = fs::read(&path).unwrap();
            let mut session = SessionLib::open(
                SessionId(42 + index as u64),
                root.path().join(format!("refused-session-{index}")),
                ModuleEnv::standalone_default(),
            )
            .unwrap();
            assert!(
                matches!(session.attach_recovery_graph_v2(&path), Err(crate::session::SessionError::RecoveryManifest { detail, .. }) if detail.contains("live dependencies differ from verified original native requirements"))
            );
            assert_eq!(session.generation(), Generation(0));
            assert_eq!(fs::read(&path).unwrap(), bytes);
        }

        let RecoveryArtifactClosure::Home(reference) = &original else {
            unreachable!()
        };
        fs::write(
            root.path().join(&reference.product_path),
            b"tampered after successful hydration",
        )
        .unwrap();
        let before = fs::read(&manifest).unwrap();
        assert!(accepted
            .validate_recovered_public_owner(&owner("root"))
            .is_err());
        assert_eq!(fs::read(&manifest).unwrap(), before);
    }

    #[test]
    fn v8_manifest_refuses_persisted_native_relation_rows() {
        let baseline = fixture();
        let original = home_artifact(&baseline).artifact_id();
        for dependency in [
            ArtifactDependency::NativeGroup {
                dependent_ordinal: 1,
                required_ordinal: 7,
            },
            ArtifactDependency::NativeBinding {
                dependent_ordinal: 1,
                generation: 7,
                namespace: "value".into(),
                occurrence: "x".into(),
                record_parent: None,
            },
        ] {
            let mut graph = baseline.clone();
            graph
                .artifact_dependencies
                .push(RecoveryArtifactDependency {
                    source: original,
                    target: original,
                    dependency,
                });
            assert_eq!(
                graph.seal().unwrap_err().detail,
                "native recovery edges must derive from original certification"
            );
        }
    }

    #[test]
    fn published_exact_root_requires_its_run_owner_on_restart() {
        use crate::session::{ModuleEnv, SessionId, SessionLib};

        let root = tempfile::tempdir().unwrap();
        let manifest = root.path().join("declarations.json");
        let graph = compiled_fixture(root.path());
        assert!(matches!(
            stage_v2(&manifest, root.path(), snapshot(&graph))
                .unwrap()
                .publish(),
            RecoveryPublishOutcome::Durable { .. }
        ));
        let mut reopened = SessionLib::open(
            SessionId(41),
            root.path().join("session"),
            ModuleEnv::standalone_default(),
        )
        .unwrap();
        assert!(matches!(reopened.attach_recovery_graph_v2(&manifest),
            Err(crate::session::SessionError::RecoveryManifest { detail, .. })
                if detail.contains("retained public recovery requires its configured run owner")));
        assert_eq!(reopened.generation(), Generation(0));
        assert!(reopened.current_module().is_none());
    }

    #[test]
    fn private_recovery_attach_refuses_corrupt_owned_artifact() {
        use crate::session::{ModuleEnv, SessionId, SessionLib};

        let root = tempfile::tempdir().unwrap();
        let manifest = root.path().join("declarations.json");
        let mut graph = compiled_fixture(root.path());
        graph.public_surfaces.clear();
        graph.nodes.retain(|node| node.id == Generation(1));
        graph.seal().unwrap();
        assert!(matches!(
            stage_v2(&manifest, root.path(), snapshot(&graph))
                .unwrap()
                .publish(),
            RecoveryPublishOutcome::Durable { .. }
        ));
        let mut control = SessionLib::open(
            SessionId(40),
            root.path().join("control-session"),
            ModuleEnv::standalone_default(),
        )
        .unwrap();
        control.attach_recovery_graph_v2(&manifest).unwrap();
        let RecoveryArtifactClosure::Home(reference) = home_artifact(&graph) else {
            panic!("fixture has a home artifact")
        };
        fs::write(root.path().join(&reference.interface_path), b"changed").unwrap();
        let mut reopened = SessionLib::open(
            SessionId(41),
            root.path().join("session"),
            ModuleEnv::standalone_default(),
        )
        .unwrap();
        assert!(matches!(reopened.attach_recovery_graph_v2(&manifest),
            Err(crate::session::SessionError::RecoveryManifest { detail, .. })
                if detail.contains("unavailable or corrupt artifacts")));
        assert_eq!(reopened.generation(), Generation(0));
    }

    #[test]
    fn binding_only_publication_is_durable_and_restarts_as_a_winner_tombstone() {
        let dir = tempfile::tempdir().unwrap();
        let manifest = dir.path().join("declarations.json");
        let original = RecoveryGraphWire::empty(41, 99).unwrap();
        let root = owner("root");
        let first = RecoveryPublicBinding {
            name: "answer".into(),
            owner: RecoveryBindingId {
                session: 41,
                variable: 1,
            },
        };
        let staged = stage_public_visibility_v2(
            &manifest,
            dir.path(),
            &snapshot(&original),
            root.clone(),
            0,
            vec![first],
            vec![],
            None,
            RecoveryCompilerContext::default(),
        )
        .unwrap();
        assert!(!manifest.exists(), "staging is not public authority");
        let published = match staged.publish() {
            RecoveryPublishOutcome::Durable { graph, .. } => graph,
            _ => panic!("expected durable publication"),
        };
        assert_eq!(
            published.public_surfaces().next().unwrap().declaration_root,
            None
        );
        assert_eq!(published.high_water(), Generation(0));
        assert_eq!(published.public_surfaces().next().unwrap().epoch, 1);
        let next = RecoveryPublicBinding {
            name: "answer".into(),
            owner: RecoveryBindingId {
                session: 41,
                variable: 2,
            },
        };
        let staged = stage_public_visibility_v2(
            &manifest,
            dir.path(),
            &published,
            root.clone(),
            1,
            vec![next.clone()],
            vec![],
            None,
            RecoveryCompilerContext::default(),
        )
        .unwrap();
        let replacement = match staged.publish() {
            RecoveryPublishOutcome::Durable { graph, .. } => graph,
            _ => panic!("expected durable replacement"),
        };
        let restarted = read_v2(&manifest, dir.path()).unwrap().unwrap().graph;
        assert_eq!(restarted, replacement);
        let replacement_bytes = fs::read(&manifest).unwrap();
        let repeated = match stage_public_visibility_v2(
            &manifest,
            dir.path(),
            &replacement,
            root.clone(),
            2,
            vec![next.clone()],
            vec![],
            None,
            RecoveryCompilerContext::default(),
        )
        .unwrap()
        .publish()
        {
            RecoveryPublishOutcome::Durable { graph, .. } => graph,
            _ => panic!("expected durable unchanged-winner publication"),
        };
        assert_eq!(
            repeated.public_surfaces().next().unwrap().bindings,
            replacement.public_surfaces().next().unwrap().bindings
        );
        assert_eq!(repeated.public_surfaces().next().unwrap().epoch, 3);
        assert_ne!(fs::read(&manifest).unwrap(), replacement_bytes);
        assert_eq!(
            repeated.public_binding_tombstones(&root).unwrap(),
            vec![RecoveryLostPublicBinding {
                name: "answer".into(),
                winner: RecoveryBindingId {
                    session: 41,
                    variable: 2,
                },
            }]
        );
        let published_bytes = fs::read(&manifest).unwrap();
        assert!(stage_public_visibility_v2(
            &manifest,
            dir.path(),
            &repeated,
            root.clone(),
            1,
            vec![],
            vec![],
            None,
            RecoveryCompilerContext::default()
        )
        .is_err());
        assert_eq!(fs::read(&manifest).unwrap(), published_bytes);
    }

    #[test]
    fn same_session_actor_surfaces_keep_independent_winners_and_epochs() {
        let dir = tempfile::tempdir().unwrap();
        let manifest = dir.path().join("declarations.json");
        let parent = owner("root");
        let child = owner("root/child");
        let initial = RecoveryGraphWire::empty(41, 99).unwrap();
        let binding = |variable| RecoveryPublicBinding {
            name: "answer".into(),
            owner: RecoveryBindingId {
                session: 41,
                variable,
            },
        };
        let parent_graph = match stage_public_visibility_v2(
            &manifest,
            dir.path(),
            &snapshot(&initial),
            parent.clone(),
            0,
            vec![binding(1)],
            vec![],
            None,
            RecoveryCompilerContext::default(),
        )
        .unwrap()
        .publish()
        {
            RecoveryPublishOutcome::Durable { graph, .. } => graph,
            _ => panic!("parent publication must be durable"),
        };
        let both = match stage_public_visibility_v2(
            &manifest,
            dir.path(),
            &parent_graph,
            child.clone(),
            0,
            vec![binding(2)],
            vec![],
            None,
            RecoveryCompilerContext::default(),
        )
        .unwrap()
        .publish()
        {
            RecoveryPublishOutcome::Durable { graph, .. } => graph,
            _ => panic!("child publication must be durable"),
        };
        assert!(stage_public_visibility_v2(
            &manifest,
            dir.path(),
            &both,
            parent.clone(),
            0,
            vec![binding(3)],
            vec![],
            None,
            RecoveryCompilerContext::default()
        )
        .is_err());
        let restored = read_v2(&manifest, dir.path()).unwrap().unwrap().graph;
        assert_eq!(restored.public_surfaces().count(), 2);
        assert_eq!(
            restored.public_binding_tombstones(&parent).unwrap()[0]
                .winner
                .variable,
            1
        );
        assert_eq!(
            restored.public_binding_tombstones(&child).unwrap()[0]
                .winner
                .variable,
            2
        );
        assert_eq!(
            restored
                .public_surfaces()
                .map(|surface| surface.epoch)
                .collect::<Vec<_>>(),
            vec![1, 1]
        );
    }

    #[test]
    fn sibling_actors_project_their_own_declaration_roots() {
        let mut graph = fixture();
        graph.public_surfaces[0].declaration_root = Some(Generation(1));
        graph.public_surfaces.push(RecoveryPublicSurface {
            owner: owner("root/child"),
            declaration_root: Some(Generation(2)),
            epoch: 0,
            bindings: vec![],
            source_instances: vec![],
            compiler_context: RecoveryCompilerContext::default(),
        });
        graph.seal().unwrap();
        let parent = snapshot(&graph)
            .projection(&owner("root"), &BTreeMap::new())
            .unwrap();
        let child = snapshot(&graph)
            .projection(&owner("root/child"), &BTreeMap::new())
            .unwrap();
        assert!(matches!(
            parent.get(&identity("answer")),
            Some(RecoveryHead::Available {
                winner: Generation(1),
                ..
            })
        ));
        assert!(matches!(
            child.get(&identity("answer")),
            Some(RecoveryHead::Tombstone(t)) if t.winner == Generation(2)
        ));
    }

    #[test]
    fn duplicate_or_invalid_actor_surfaces_are_refused() {
        let mut graph = RecoveryGraphWire::empty(41, 99).unwrap();
        let surface = RecoveryPublicSurface {
            owner: owner("root"),
            declaration_root: None,
            epoch: 0,
            bindings: vec![],
            source_instances: vec![],
            compiler_context: RecoveryCompilerContext::default(),
        };
        graph.public_surfaces = vec![surface.clone(), surface];
        assert!(graph.seal().is_err());
        graph.public_surfaces.pop();
        graph.public_surfaces[0].owner.path = "Root".into();
        assert!(graph.seal().is_err());
    }

    #[test]
    fn old_recovery_formats_are_refused_without_rewriting_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let manifest = dir.path().join("declarations.json");
        let current = serde_json::to_value(snapshot(&compiled_fixture(dir.path()))).unwrap();
        for version in 1..u64::from(VERSION) {
            let mut record = current.clone();
            record["version"] = serde_json::json!(version);
            let bytes = serde_json::to_vec(&record).unwrap();
            fs::write(&manifest, &bytes).unwrap();

            let error = read_v2(&manifest, dir.path()).err().unwrap();

            assert_eq!(
                error.kind,
                RecoveryErrorKind::Format(RecoveryRefusal::UnsupportedOldFormat { version })
            );
            assert_eq!(fs::read(&manifest).unwrap(), bytes);
        }
    }

    #[test]
    fn native_recovery_requires_the_exact_canonical_companion_and_selection() {
        let baseline = fixture();
        for mutation in 0..4 {
            let mut wire = baseline.clone();
            let RecoveryArtifactClosure::Home(home) = home_artifact(&wire) else {
                unreachable!()
            };
            let canonical = ArtifactDescriptor::from_recovery_module_interface(
                home.module_interface.as_ref().unwrap(),
            )
            .id;
            match mutation {
                0 => {
                    let RecoveryArtifactClosure::Home(home) = home_artifact_mut(&mut wire) else {
                        unreachable!()
                    };
                    home.module_interface = None;
                }
                1 => wire
                    .artifacts
                    .retain(|artifact| artifact.artifact_id() != canonical),
                2 => wire.artifact_dependencies.clear(),
                3 => wire.nodes[0].artifact_refs.retain(|id| *id != canonical),
                _ => unreachable!(),
            }
            assert!(
                wire.seal().is_err(),
                "accepted incomplete native companion mutation {mutation}"
            );
        }
    }

    #[test]
    fn canonical_interface_closure_recovers_without_a_native_product() {
        let dir = tempfile::tempdir().unwrap();
        let mut wire = compiled_fixture(dir.path());
        let RecoveryArtifactClosure::Home(home) = home_artifact(&wire) else {
            unreachable!()
        };
        let interface = home.module_interface.clone().unwrap();
        let product_path = home.product_path.clone();
        let canonical = RecoveryArtifactClosure::ModuleInterface(interface.clone());
        wire.nodes[0].artifact_refs = vec![canonical.artifact_id()];
        wire.nodes[0].compiler_roles = vec![
            tidepool_toolchain::artifact_inventory::CompilerInputRole::InterfaceOnly {
                interface: canonical.artifact_id(),
            },
        ];
        wire.nodes[0].native_groups.clear();
        wire.artifacts = vec![canonical];
        wire.artifact_dependencies.clear();
        wire.seal().unwrap();
        fs::remove_file(dir.path().join(product_path)).unwrap();

        let bytes = serde_json::to_vec(&wire).unwrap();
        let recovered = read_v2_bytes(
            &dir.path().join("graph.json"),
            dir.path(),
            &bytes,
            RecoveryReadPurpose::Hydration,
        )
        .unwrap()
        .unwrap();
        assert!(recovered.artifact_losses.is_empty());
        assert!(recovered.graph.capture_inventory(dir.path()).is_ok());
        assert!(matches!(
            recovered.graph.artifacts().next(),
            Some(RecoveryArtifactClosure::ModuleInterface(_))
        ));

        let core = interface.core.unwrap();
        fs::remove_file(dir.path().join(&core.path)).unwrap();
        let losses = recovered.graph.validate_artifact_files(dir.path()).unwrap();
        assert_eq!(losses.len(), 1);
        let loss = &losses.values().next().unwrap()[0];
        assert_eq!(loss.component, RecoveryArtifactComponent::Core);
        assert_eq!(loss.path, core.path);
        assert_eq!(loss.kind, RecoveryArtifactLossKind::Missing);
    }

    #[test]
    fn all_recovery_witness_paths_are_confined_before_hydration() {
        let wire = fixture();
        let home = home_artifact(&wire).clone();
        let RecoveryArtifactClosure::Home(reference) = &home else {
            unreachable!()
        };
        let canonical =
            RecoveryArtifactClosure::ModuleInterface(reference.module_interface.clone().unwrap());
        let join = RecoveryArtifactClosure::Join(
            reference
                .module_interface
                .as_ref()
                .unwrap()
                .interface
                .clone(),
        );
        let value = RecoveryArtifactClosure::ValueInterface(value_interface_from(reference, "Val"));
        for original in [home, canonical, join, value] {
            let mut safe = wire.clone();
            safe.nodes[0].artifact_refs = vec![original.artifact_id()];
            safe.artifacts = vec![original.clone()];
            safe.artifact_dependencies.clear();
            safe.nodes[0].compiler_roles.clear();
            if matches!(original, RecoveryArtifactClosure::Home(_)) {
                install_fixture_canonical_interfaces(&mut safe);
            } else {
                safe.nodes[0].compiler_roles = vec![
                    tidepool_toolchain::artifact_inventory::CompilerInputRole::InterfaceOnly {
                        interface: original.artifact_id(),
                    },
                ];
            }
            safe.seal().unwrap();
            for (component, path) in original.component_paths() {
                for unsafe_path in [
                    PathBuf::new(),
                    PathBuf::from("../escaped"),
                    PathBuf::from("/escaped"),
                    PathBuf::from("dir/../escaped"),
                ] {
                    let mut graph = wire.clone();
                    // Replace the exact encoded payload path, including a nested
                    // Home canonical companion, without skipping descriptor checks.
                    let encoded = serde_json::to_value(&original).unwrap();
                    fn replace(value: &mut serde_json::Value, old: &str, new: &str) {
                        match value {
                            serde_json::Value::String(text) if text == old => *text = new.into(),
                            serde_json::Value::Array(items) => {
                                for item in items {
                                    replace(item, old, new);
                                }
                            }
                            serde_json::Value::Object(items) => {
                                for item in items.values_mut() {
                                    replace(item, old, new);
                                }
                            }
                            _ => {}
                        }
                    }
                    let mut encoded = encoded;
                    replace(
                        &mut encoded,
                        path.to_str().unwrap(),
                        unsafe_path.to_str().unwrap(),
                    );
                    let changed: RecoveryArtifactClosure = serde_json::from_value(encoded).unwrap();
                    graph.nodes[0].artifact_refs = vec![changed.artifact_id()];
                    graph.artifacts = vec![changed];
                    graph.artifact_dependencies.clear();
                    graph.nodes[0].compiler_roles.clear();
                    // The matching safe baseline proves failure is caused by
                    // the path; unsafe native rows keep their companion.
                    if matches!(original, RecoveryArtifactClosure::Home(_)) {
                        install_fixture_canonical_interfaces(&mut graph);
                    } else {
                        graph.nodes[0].compiler_roles = vec![
                            tidepool_toolchain::artifact_inventory::CompilerInputRole::InterfaceOnly {
                                interface: graph.artifacts[0].artifact_id(),
                            },
                        ];
                    }
                    assert_eq!(
                        graph.seal().unwrap_err().detail,
                        "invalid recovery artifact reference",
                        "unsafe {component:?} path {unsafe_path:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn canonical_witness_losses_keep_the_exact_component_and_relative_path() {
        let dir = tempfile::tempdir().unwrap();
        let wire = fixture();
        let RecoveryArtifactClosure::Home(home) = home_artifact(&wire) else {
            unreachable!()
        };
        let canonical =
            RecoveryArtifactClosure::ModuleInterface(home.module_interface.clone().unwrap());
        for artifact in [home_artifact(&wire), &canonical] {
            for (component, relative) in artifact.component_paths() {
                let absolute = dir.path().join(relative);
                for (error, expected) in [
                    (
                        RecoveryArtifactError::Unavailable(absolute.clone()),
                        RecoveryArtifactLossKind::Missing,
                    ),
                    (
                        RecoveryArtifactError::DigestMismatch(absolute.clone()),
                        RecoveryArtifactLossKind::DigestMismatch,
                    ),
                    (
                        RecoveryArtifactError::Unreadable {
                            path: absolute.clone(),
                            error: std::io::Error::new(
                                std::io::ErrorKind::PermissionDenied,
                                "denied",
                            ),
                        },
                        RecoveryArtifactLossKind::Unreadable("denied".into()),
                    ),
                ] {
                    let loss = artifact_error_loss(artifact, error).unwrap();
                    assert_eq!(loss.component, component);
                    assert_eq!(loss.path, relative);
                    assert_eq!(loss.kind, expected);
                }
            }
        }
        let certificate = home
            .module_interface
            .as_ref()
            .unwrap()
            .certificate_path
            .clone();
        let invalid = artifact_error_loss(
            &canonical,
            RecoveryArtifactError::InvalidModuleCertificate(dir.path().join(&certificate)),
        )
        .unwrap();
        assert_eq!(invalid.component, RecoveryArtifactComponent::Certificate);
        assert_eq!(invalid.path, certificate);
        let core = home
            .module_interface
            .as_ref()
            .unwrap()
            .core
            .as_ref()
            .unwrap()
            .path
            .clone();
        let invalid = artifact_error_loss(
            &canonical,
            RecoveryArtifactError::InvalidCapturedPayload(dir.path().join(&core)),
        )
        .unwrap();
        assert_eq!(invalid.component, RecoveryArtifactComponent::Core);
        assert_eq!(invalid.path, core);
    }

    #[test]
    fn canonical_interface_identity_ignores_all_materialization_paths() {
        let wire = fixture();
        let RecoveryArtifactClosure::Home(home) = home_artifact(&wire) else {
            unreachable!()
        };
        let mut interface = home.module_interface.clone().unwrap();
        let id = ArtifactDescriptor::from_recovery_module_interface(&interface).id;
        interface.interface.interface_path = "relocated/module.hi".into();
        interface.interface.package_imports_path = "relocated/module.packages".into();
        interface.certificate_path = "relocated/module.certificate".into();
        interface.core.as_mut().unwrap().path = "relocated/module.core".into();
        assert_eq!(
            ArtifactDescriptor::from_recovery_module_interface(&interface).id,
            id
        );
        interface.certificate_sha256[0] ^= 1;
        assert_ne!(
            ArtifactDescriptor::from_recovery_module_interface(&interface).id,
            id
        );
    }

    #[test]
    fn artifact_ids_ignore_materialization_paths() {
        let graph = fixture();
        let RecoveryArtifactClosure::Home(mut reference) = home_artifact(&graph).clone() else {
            panic!("fixture has an original module artifact")
        };
        let id = ArtifactDescriptor::from_recovery_product(&reference).id;
        reference.interface_path = PathBuf::from("relocated/Lib.hi");
        reference.package_imports_path = PathBuf::from("relocated/Lib.packages");
        reference.certification_path = PathBuf::from("relocated/Lib.owners");
        reference.product_path = PathBuf::from("relocated/Lib.product");
        assert_eq!(ArtifactDescriptor::from_recovery_product(&reference).id, id);
    }

    #[test]
    fn value_interface_artifact_id_and_exact_requirements_are_validated() {
        let mut graph = fixture();
        let RecoveryArtifactClosure::Home(home) = home_artifact(&graph).clone() else {
            unreachable!()
        };
        let mut value = value_interface_from(&home, "Val");
        value.requirements.push(module("main", "Lib"));
        let value_id = value.artifact_id;
        graph
            .artifacts
            .push(RecoveryArtifactClosure::ValueInterface(value.clone()));
        graph.nodes[0].artifact_refs.push(value_id);
        assert!(graph
            .seal()
            .unwrap_err()
            .detail
            .contains("without its direct artifact edge"));

        graph
            .artifact_dependencies
            .push(RecoveryArtifactDependency {
                source: value_id,
                target: ArtifactDescriptor::from_recovery_module_interface(
                    home.module_interface.as_ref().unwrap(),
                )
                .id,
                dependency: ArtifactDependency::Interface,
            });
        graph.seal().unwrap();

        let value = graph
            .artifacts
            .iter_mut()
            .find_map(|artifact| match artifact {
                RecoveryArtifactClosure::ValueInterface(value) => Some(value),
                _ => None,
            })
            .expect("fixture retains its value interface");
        value.artifact_id = ArtifactId([0xff; 32]);
        assert!(graph
            .seal()
            .unwrap_err()
            .detail
            .contains("artifact ID does not match its descriptor"));
    }

    #[test]
    fn future_recovery_format_is_distinctly_refused_without_rewriting_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let manifest = dir.path().join("declarations.json");
        let version = u64::MAX;
        let bytes = format!("{{\"version\":{version},\"future\":true}}\n").into_bytes();
        fs::write(&manifest, &bytes).unwrap();

        let error = read_v2(&manifest, dir.path()).err().unwrap();

        assert_eq!(
            error.kind,
            RecoveryErrorKind::Format(RecoveryRefusal::UnsupportedFutureFormat { version })
        );
        assert_eq!(fs::read(&manifest).unwrap(), bytes);
    }

    #[test]
    fn family_only_inventory_retains_hidden_consistency_evidence() {
        let mut graph = fixture();
        graph.nodes[0].instances = RecoveryInstanceInventory {
            classes: Vec::new(),
            selected_family_axioms: vec![identity("selectedAxiom")],
            family_consistency_closure: vec![identity("selectedAxiom"), identity("hiddenAxiom")],
        };
        graph.seal().unwrap();
        let bytes = serde_json::to_vec(&graph).unwrap();
        let read = RecoveryGraph::from_wire(serde_json::from_slice(&bytes).unwrap()).unwrap();
        assert_eq!(
            read.nodes().next().unwrap().instances,
            graph.nodes[0].instances
        );
        assert!(read.nodes().next().unwrap().instances.classes.is_empty());
        let mut changed = read.wire_for_test();
        changed.nodes[0].instances.family_consistency_closure.pop();
        assert!(changed.validate().is_err());
    }

    #[test]
    fn selected_family_and_associated_axioms_require_exact_inventory_membership() {
        let mut graph = fixture();
        graph.nodes[0].instances.selected_family_axioms = vec![identity("axiom")];
        assert!(graph.seal().unwrap_err().detail.contains("family evidence"));
        graph.nodes[0].instances.family_consistency_closure = vec![identity("axiom")];
        graph.nodes[0].instances.classes = vec![RecoveryInstanceEvidence {
            dfun: identity("dfun"),
            class: identity("class"),
            selected: true,
            selected_axioms: vec![identity("otherAxiom")],
        }];
        assert!(graph
            .seal()
            .unwrap_err()
            .detail
            .contains("instance evidence"));
        graph.nodes[0].instances.classes[0].selected_axioms = vec![identity("axiom")];
        graph.seal().unwrap();
        let repeated = graph.nodes[0].instances.classes[0].clone();
        graph.nodes[0].instances.classes.push(repeated);
        assert!(graph
            .seal()
            .unwrap_err()
            .detail
            .contains("instance evidence"));
    }

    #[test]
    fn lexical_graph_requires_closed_reachable_artifact_owned_identities() {
        let valid = module("main", "Lib");
        let missing = module("main", "Missing");

        let mut graph = fixture();
        graph.nodes[0].lexical_roots = vec![valid.clone()];
        graph.nodes[0].lexical = vec![ExactLexicalNode {
            owner: valid.clone(),
            imports: vec![],
        }];
        graph.seal().unwrap();

        let mut unrepresented = graph.clone();
        unrepresented.nodes[0].lexical_roots = vec![missing.clone()];
        assert!(unrepresented
            .seal()
            .unwrap_err()
            .detail
            .contains("unrepresented lexical roots"));

        let mut open_edge = graph.clone();
        open_edge.nodes[0].lexical[0].imports.push(missing);
        assert!(open_edge
            .seal()
            .unwrap_err()
            .detail
            .contains("non-closed lexical edges"));

        let mut unreachable = graph.clone();
        unreachable.nodes[0].lexical_roots.clear();
        assert!(unreachable
            .seal()
            .unwrap_err()
            .detail
            .contains("outside its exact root closure"));

        let mut unowned = graph;
        unowned.nodes[0].lexical[0].owner = module("other", "Lib");
        unowned.nodes[0].lexical_roots = vec![module("other", "Lib")];
        assert!(unowned
            .seal()
            .unwrap_err()
            .detail
            .contains("without a referenced exact artifact"));
    }

    #[test]
    fn v8_checksum_covers_lexical_roots_and_edges() {
        let root = module("main", "Lib");
        let other = module("main", "Other");
        let mut graph = fixture();
        let RecoveryArtifactClosure::Home(mut other_artifact) = home_artifact(&graph).clone()
        else {
            unreachable!()
        };
        other_artifact.module = "Other".into();
        other_artifact
            .module_interface
            .as_mut()
            .unwrap()
            .interface
            .module = "Other".into();
        let other_closure = RecoveryArtifactClosure::Home(other_artifact);
        let other_key = other_closure.artifact_id();
        graph.artifacts.push(other_closure);
        graph.nodes[0].artifact_refs.push(other_key);
        install_fixture_canonical_interfaces(&mut graph);
        graph.nodes[0].lexical_roots = vec![root.clone(), other.clone()];
        graph.nodes[0].lexical = vec![
            ExactLexicalNode {
                owner: root.clone(),
                imports: vec![other.clone()],
            },
            ExactLexicalNode {
                owner: other,
                imports: vec![],
            },
        ];
        graph.seal().unwrap();
        graph.validate().unwrap();

        let mut roots_changed = graph.clone();
        roots_changed.nodes[0].lexical_roots.reverse();
        assert!(roots_changed
            .validate()
            .unwrap_err()
            .detail
            .contains("checksum mismatch"));

        let mut edges_changed = graph;
        edges_changed.nodes[0].lexical[0].imports = vec![root];
        assert!(edges_changed
            .validate()
            .unwrap_err()
            .detail
            .contains("checksum mismatch"));
    }

    #[test]
    fn checksum_covers_the_graph_and_projection_preserves_lost_winner_tombstones() {
        let graph = fixture();
        let bytes = serde_json::to_vec(&graph).unwrap();
        let decoded: RecoveryGraphWire = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(decoded, graph);
        decoded.validate().unwrap();
        let projected = snapshot(&graph)
            .projection(&owner("root"), &BTreeMap::new())
            .unwrap();
        assert!(
            matches!(projected.get(&identity("answer")), Some(RecoveryHead::Tombstone(t)) if t.winner == Generation(2))
        );

        let mut changed = graph;
        changed.nodes[1].state = RecoveryNodeState::ExactArtifactClosure;
        assert!(changed.validate().is_err());
    }

    #[test]
    fn artifact_validation_shares_real_package_bytes_and_rechecks_each_read() {
        use tidepool_toolchain::recovery_artifacts::{
            materialize_joined_interface, verify_materialized_join_with_work,
            verify_materialized_ref_with_work, with_recovery_artifact_verification,
        };

        let dir = tempfile::tempdir().unwrap();
        let mut wire = compiled_fixture(dir.path());
        let RecoveryArtifactClosure::Home(home) = home_artifact_mut(&mut wire) else {
            unreachable!()
        };
        let join = materialize_joined_interface(
            dir.path(),
            home.toolchain_identity_sha256,
            &home.unit,
            &home.module,
            &dir.path().join(&home.interface_path),
            home.skinny_iface_sha256,
        )
        .unwrap();
        let certification_path = dir.path().join(&home.certification_path);
        let mut separate = RecoveryArtifactWork::default();
        verify_materialized_ref_with_work(dir.path(), home, &mut separate).unwrap();
        verify_materialized_join_with_work(dir.path(), &join, &mut separate).unwrap();
        with_recovery_artifact_verification(dir.path(), &mut separate, |verification| {
            verification.verify_module_interface(home.module_interface.as_ref().unwrap())
        })
        .unwrap();
        wire.artifacts
            .retain(|artifact| !matches!(artifact, RecoveryArtifactClosure::ModuleInterface(_)));
        wire.artifact_dependencies.clear();
        install_fixture_canonical_interfaces(&mut wire);
        let home_id = home_artifact(&wire).artifact_id();
        let join = RecoveryArtifactClosure::Join(join);
        let join_id = join.artifact_id();
        wire.nodes[0].artifact_refs = vec![home_id];
        install_fixture_canonical_interfaces(&mut wire);
        wire.nodes[1].artifact_refs = vec![join_id];
        wire.nodes[1].compiler_roles = vec![
            tidepool_toolchain::artifact_inventory::CompilerInputRole::InterfaceOnly {
                interface: join_id,
            },
        ];
        wire.nodes[1].state = RecoveryNodeState::ExactArtifactClosure;
        wire.artifacts.push(join);
        wire.seal().unwrap();
        let graph = snapshot(&wire);
        let mut batched = RecoveryArtifactWork::default();
        assert!(graph
            .validate_artifact_files_with_work(dir.path(), &mut batched)
            .unwrap()
            .is_empty());
        assert!(
            separate.hash_bytes > batched.hash_bytes,
            "batched verification must reuse the compiler's actual package witnesses"
        );

        let bytes = serde_json::to_vec(&graph).unwrap();
        let path = dir.path().join("graph.json");
        fs::write(&certification_path, b"corrupt").unwrap();
        let recovered = read_v2_bytes(&path, dir.path(), &bytes, RecoveryReadPurpose::Metadata)
            .unwrap()
            .unwrap();
        let losses = recovered.artifact_losses;
        assert_eq!(losses.len(), 1);
        assert!(losses.contains_key(&home_id));
    }

    #[test]
    fn staged_publication_work_counts_actual_checksum_hash_and_write_bytes() {
        use tidepool_toolchain::recovery_artifacts::with_recovery_artifact_verification;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("graph.json");
        let baseline = snapshot(&compiled_fixture(dir.path()));
        let home = baseline
            .artifacts()
            .find_map(|artifact| match artifact {
                RecoveryArtifactClosure::Home(home) => Some(home),
                _ => None,
            })
            .unwrap();
        // SHA work includes the owner's independent native/canonical checks;
        // shared component paths do not imply that each payload is hashed once.
        let mut expected_validation = RecoveryArtifactWork::default();
        with_recovery_artifact_verification(dir.path(), &mut expected_validation, |verification| {
            verification.verify_home(home)?;
            verification.verify_module_interface(home.module_interface.as_ref().unwrap())
        })
        .unwrap();
        let staged = stage_public_visibility_v2(
            &path,
            dir.path(),
            &baseline,
            owner("root"),
            0,
            Vec::new(),
            Vec::new(),
            None,
            RecoveryCompilerContext::default(),
        )
        .unwrap();
        let unsigned = serde_json::to_vec(&GraphEncoding {
            graph: &staged.graph,
            checksum: "",
        })
        .unwrap();
        let encoded = serde_json::to_vec(&staged.graph).unwrap();
        let expected_hash_bytes = expected_validation.hash_bytes;
        assert_eq!(staged.work.checksum_encode_bytes, unsigned.len() as u64);
        assert_eq!(
            staged.work.recovery_validation_hash_bytes,
            expected_hash_bytes
        );
        assert_eq!(staged.work.recovery_materialization_hash_bytes, 0);
        assert_eq!(staged.work.manifest_write_bytes, encoded.len() as u64);
        assert!(staged.work.checksum_encode_bytes > 0 && expected_hash_bytes > 0);
        let RecoveryPublishOutcome::Durable { graph, .. } = staged.publish() else {
            panic!("durable fixture")
        };
        assert_eq!(fs::read(&path).unwrap(), encoded);
        // Validated existing graphs perform no checksum encoding while staging.
        let restaged = stage_v2(&path, dir.path(), graph).unwrap();
        assert_eq!(restaged.work.checksum_encode_bytes, 0);
        assert_eq!(
            restaged.work.recovery_validation_hash_bytes,
            expected_hash_bytes
        );
        assert_eq!(restaged.work.manifest_write_bytes, encoded.len() as u64);
        let mut wire = compiled_fixture(dir.path());
        wire.nodes.reverse();
        wire.checksum = checksum(&wire).unwrap();
        let noncanonical = snapshot(&wire);
        let canonicalized = stage_v2(&path, dir.path(), noncanonical).unwrap();
        let canonical_unsigned = serde_json::to_vec(&GraphEncoding {
            graph: &canonicalized.graph,
            checksum: "",
        })
        .unwrap();
        assert_eq!(
            canonicalized.work.checksum_encode_bytes,
            canonical_unsigned.len() as u64
        );
    }

    #[test]
    fn checksum_matches_the_canonical_unsigned_graph_encoding() {
        let graph = fixture();
        let mut unsigned = graph.clone();
        unsigned.checksum.clear();
        let bytes = serde_json::to_vec(&unsigned).unwrap();
        let mut domain = b"tidepool-recovery-graph-v9\0".to_vec();
        domain.extend_from_slice(&bytes);
        assert_eq!(
            checksum(&graph).unwrap(),
            blake3::hash(&domain).to_hex().to_string()
        );
    }

    #[test]
    fn artifact_paths_must_stay_relative_to_the_recovery_root() {
        let mut graph = fixture();
        match home_artifact_mut(&mut graph) {
            RecoveryArtifactClosure::Home(reference) => {
                reference.interface_path = PathBuf::from("../escape.hi");
            }
            RecoveryArtifactClosure::Join(_)
            | RecoveryArtifactClosure::ModuleInterface(_)
            | RecoveryArtifactClosure::ValueInterface(_) => {
                unreachable!()
            }
        }
        assert!(graph.seal().unwrap_err().detail.contains("artifact"));
    }

    #[test]
    fn reservations_advance_exactly_one_generation_without_publishing_a_node() {
        let graph = fixture();
        let candidate = high_water_candidate(&snapshot(&graph), Generation(3))
            .unwrap()
            .0;
        assert_eq!(candidate.high_water(), Generation(3));
        assert_eq!(
            candidate
                .public_surfaces()
                .map(|surface| surface.clone())
                .collect::<Vec<_>>(),
            graph.public_surfaces
        );
        assert_eq!(
            candidate
                .nodes()
                .map(|node| node.clone())
                .collect::<Vec<_>>(),
            graph.nodes
        );
        assert!(high_water_candidate(&snapshot(&graph), Generation(4)).is_err());
    }

    #[test]
    fn staged_high_water_is_invisible_until_publish_and_survives_readback() {
        let dir = tempfile::tempdir().unwrap();
        let graph = RecoveryGraphWire::empty(41, 99).unwrap();
        let manifest_path = dir.path().join("recovery.json");
        fs::write(&manifest_path, b"previous manifest").unwrap();

        let staged = stage_high_water_v2(&manifest_path, &snapshot(&graph), Generation(3)).unwrap();
        assert_eq!(staged.candidate_graph().high_water(), Generation(3));
        assert_eq!(fs::read(&manifest_path).unwrap(), b"previous manifest");
        drop(staged);
        assert_eq!(fs::read(&manifest_path).unwrap(), b"previous manifest");

        match stage_high_water_v2(&manifest_path, &snapshot(&graph), Generation(3))
            .unwrap()
            .publish()
        {
            RecoveryPublishOutcome::Durable {
                graph: published,
                publication,
            } => {
                assert_eq!(publication.path(), manifest_path);
                assert_eq!(published.high_water(), Generation(3));
                assert_eq!(
                    published
                        .public_surfaces()
                        .map(|surface| surface.clone())
                        .collect::<Vec<_>>(),
                    graph.public_surfaces
                );
                assert_eq!(
                    published
                        .nodes()
                        .map(|node| node.clone())
                        .collect::<Vec<_>>(),
                    graph.nodes
                );
            }
            RecoveryPublishOutcome::BeforeRename { path, detail } => {
                panic!(
                    "unexpected pre-rename failure at {}: {detail}",
                    path.display()
                )
            }
            RecoveryPublishOutcome::PublishedDurabilityUnconfirmed { detail, .. } => {
                panic!("unexpected durability uncertainty: {detail}")
            }
        }

        let restored = read_v2(&manifest_path, dir.path()).unwrap().unwrap();
        assert_eq!(restored.graph.high_water(), Generation(3));
        assert_eq!(
            restored
                .graph
                .public_surfaces()
                .map(|surface| surface.clone())
                .collect::<Vec<_>>(),
            graph.public_surfaces
        );
        assert!(restored.artifact_losses.is_empty());
    }

    #[test]
    fn pre_rename_failure_keeps_target_unpublished() {
        let dir = tempfile::tempdir().unwrap();
        let graph = fixture();
        let manifest_path = dir.path().join("recovery.json");
        let staged = stage_high_water_v2(&manifest_path, &snapshot(&graph), Generation(3)).unwrap();
        fs::create_dir(&manifest_path).unwrap();

        match staged.publish() {
            RecoveryPublishOutcome::BeforeRename { path, .. } => {
                assert_eq!(path, manifest_path);
                assert!(manifest_path.is_dir());
            }
            RecoveryPublishOutcome::Durable { .. }
            | RecoveryPublishOutcome::PublishedDurabilityUnconfirmed { .. } => {
                panic!("a directory target must reject file publication before rename")
            }
        }
    }

    #[test]
    fn artifact_bytes_are_checked_against_the_manifest_digests() {
        let dir = tempfile::tempdir().unwrap();
        let graph = compiled_fixture(dir.path());
        let manifest = dir.path().join("recovery.json");
        assert!(matches!(
            stage_v2(&manifest, dir.path(), snapshot(&graph))
                .unwrap()
                .publish(),
            RecoveryPublishOutcome::Durable { .. }
        ));
        let published = fs::read(&manifest).unwrap();
        let artifact_id = home_artifact(&graph).artifact_id();
        let product = match home_artifact(&graph) {
            RecoveryArtifactClosure::Home(reference) => reference.product_path.clone(),
            RecoveryArtifactClosure::Join(_)
            | RecoveryArtifactClosure::ModuleInterface(_)
            | RecoveryArtifactClosure::ValueInterface(_) => {
                unreachable!()
            }
        };
        fs::write(dir.path().join(product), b"changed").unwrap();
        assert!(stage_v2(&manifest, dir.path(), snapshot(&graph)).is_err());
        assert_eq!(fs::read(&manifest).unwrap(), published);
        let losses = snapshot(&graph)
            .validate_artifact_files(dir.path())
            .unwrap();
        assert!(losses.contains_key(&artifact_id));
        assert!(losses
            .values()
            .flatten()
            .any(|loss| matches!(&loss.kind, RecoveryArtifactLossKind::DigestMismatch)));
    }

    #[test]
    fn a_missing_winning_artifact_becomes_a_tombstone_without_resurrection() {
        let dir = tempfile::tempdir().unwrap();
        let mut graph = compiled_fixture(dir.path());
        graph.nodes[1].state = RecoveryNodeState::ExactArtifactClosure;
        graph.nodes[1].artifact_refs = graph.nodes[0].artifact_refs.clone();
        graph.nodes[1].native_groups = graph.nodes[0].native_groups.clone();
        graph.nodes[1].compiler_roles = graph.nodes[0].compiler_roles.clone();
        graph.seal().unwrap();
        let product = match home_artifact(&graph) {
            RecoveryArtifactClosure::Home(reference) => reference.product_path.clone(),
            RecoveryArtifactClosure::Join(_)
            | RecoveryArtifactClosure::ModuleInterface(_)
            | RecoveryArtifactClosure::ValueInterface(_) => {
                unreachable!()
            }
        };
        fs::remove_file(dir.path().join(product)).unwrap();
        let losses = snapshot(&graph)
            .validate_artifact_files(dir.path())
            .unwrap();
        let projected = snapshot(&graph)
            .projection(&owner("root"), &losses)
            .unwrap();
        assert!(
            matches!(projected.get(&identity("answer")), Some(RecoveryHead::Tombstone(t)) if t.winner == Generation(2))
        );
    }

    #[test]
    fn restart_read_keeps_missing_winning_artifact_as_tombstone() {
        let dir = tempfile::tempdir().unwrap();
        let mut graph = compiled_fixture(dir.path());
        graph.nodes[1].state = RecoveryNodeState::ExactArtifactClosure;
        graph.nodes[1].artifact_refs = graph.nodes[0].artifact_refs.clone();
        graph.nodes[1].native_groups = graph.nodes[0].native_groups.clone();
        graph.nodes[1].compiler_roles = graph.nodes[0].compiler_roles.clone();
        graph.seal().unwrap();
        let manifest_path = dir.path().join("recovery.json");
        match stage_v2(&manifest_path, dir.path(), snapshot(&graph))
            .unwrap()
            .publish()
        {
            RecoveryPublishOutcome::Durable { .. } => {}
            RecoveryPublishOutcome::BeforeRename { path, detail } => {
                panic!(
                    "unexpected pre-rename failure at {}: {detail}",
                    path.display()
                )
            }
            RecoveryPublishOutcome::PublishedDurabilityUnconfirmed { detail, .. } => {
                panic!("unexpected durability uncertainty: {detail}")
            }
        }
        let product = match home_artifact(&graph) {
            RecoveryArtifactClosure::Home(reference) => reference.product_path.clone(),
            RecoveryArtifactClosure::Join(_)
            | RecoveryArtifactClosure::ModuleInterface(_)
            | RecoveryArtifactClosure::ValueInterface(_) => {
                unreachable!()
            }
        };
        fs::remove_file(dir.path().join(product)).unwrap();

        let restored = read_v2(&manifest_path, dir.path()).unwrap().unwrap();
        assert!(restored.artifact_losses.values().flatten().any(|loss| {
            loss.component == RecoveryArtifactComponent::Product
                && matches!(&loss.kind, RecoveryArtifactLossKind::Missing)
        }));
        let projected = restored.projection(&owner("root")).unwrap();
        assert!(matches!(
            projected.get(&identity("answer")),
            Some(RecoveryHead::Tombstone(t)) if t.winner == Generation(2)
        ));
    }

    #[test]
    fn missing_replacement_tombstone_retracts_old_original_identity() {
        let mut graph = fixture();
        let old = identity_in("G1", "foo");
        let replacement = identity_in("G2", "foo");
        graph.nodes[0].exports[0].identity = old.clone();
        graph.nodes[1].exports[0].identity = replacement.clone();
        graph.nodes[1].retracts = vec![old.clone()];
        graph.seal().unwrap();

        let projected = snapshot(&graph)
            .projection(&owner("root"), &BTreeMap::new())
            .unwrap();
        assert!(!projected.contains_key(&old));
        assert!(matches!(
            projected.get(&replacement),
            Some(RecoveryHead::Tombstone(t)) if t.winner == Generation(2)
        ));
    }

    #[test]
    fn workbench_imports_fold_in_lexical_order_and_keep_exact_specs() {
        let mut graph = fixture();
        graph.nodes[1]
            .workbench_imports
            .push("qualified Data.Map.Strict as Map".into());
        graph.seal().unwrap();
        assert_eq!(
            snapshot(&graph).workbench_imports(Generation(2)).unwrap(),
            [
                "qualified Data.Map.Strict as Map",
                "Data.Proxy (Proxy (..))"
            ]
        );
    }
}
