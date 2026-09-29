//! Checksummed declaration recovery graph. The manifest is metadata only;
//! executable bytes stay owned by the toolchain cache and its run-owned copy.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path, PathBuf};
use tidepool_repr::Generation;
use tidepool_toolchain::recovery_artifacts::{
    verify_materialized_join, verify_materialized_ref, RecoveryArtifactError, RecoveryArtifactRef,
    RecoveryJoinRef,
};

const VERSION: u32 = 2;
const PAIRED_PUBLIC_SCHEMA: &str = "paired-public-v1";
const MAX_MANIFEST_BYTES: usize = 64 << 20;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RecoveryGraph {
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
    pub checksum: String,
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
    pub artifact_refs: Vec<String>,
    pub exports: Vec<RecoveryExport>,
    pub retracts: Vec<RecoverySymbolIdentity>,
    /// GHC-normalized import specifications introduced by this turn. These
    /// are metadata for rebuilding the next workbench context, never replayed
    /// as declaration source.
    pub workbench_imports: Vec<String>,
    pub instances: Vec<RecoveryInstanceEvidence>,
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
}

impl RecoveryArtifactClosure {
    #[must_use]
    pub(crate) fn key(&self) -> String {
        let mut hasher = blake3::Hasher::new();
        frame(&mut hasher, b"tidepool-recovery-artifact-ref-v2");
        let bytes = serde_json::to_vec(self).expect("artifact ref is serializable");
        frame(&mut hasher, &bytes);
        hasher.finalize().to_hex().to_string()
    }

    fn paths(&self) -> (&Path, Option<&Path>) {
        match self {
            Self::Home(reference) => (&reference.interface_path, Some(&reference.product_path)),
            Self::Join(reference) => (&reference.interface_path, None),
        }
    }
}

fn frame(hasher: &mut blake3::Hasher, bytes: &[u8]) {
    hasher.update(&(bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
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
    /// Full consistency closure is retained independently of the selected
    /// reduction surface, so a hidden incompatible family axiom stays known.
    pub family_consistency_closure: Vec<RecoverySymbolIdentity>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "dependency", rename_all = "snake_case")]
pub(crate) enum RecoveryLiveDependency {
    Binding {
        binding: RecoveryBindingId,
        name: String,
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
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum RecoveryArtifactLossKind {
    Missing,
    Unreadable(String),
    DigestMismatch,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RecoveryArtifactLoss {
    pub component: RecoveryArtifactComponent,
    pub path: PathBuf,
    pub kind: RecoveryArtifactLossKind,
}

pub(crate) struct RecoveryV2Read {
    pub graph: RecoveryGraph,
    pub artifact_losses: BTreeMap<String, Vec<RecoveryArtifactLoss>>,
}

impl RecoveryV2Read {
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RecoveryError {
    pub path: Option<PathBuf>,
    pub detail: String,
}

pub(crate) struct StagedRecoveryManifest {
    inner: tidepool_atomic_write::StagedDurableWrite,
    graph: RecoveryGraph,
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
    graph: &RecoveryGraph,
) -> Result<StagedRecoveryManifest, RecoveryError> {
    graph.validate()?;
    if !graph.validate_artifact_files(recovery_root)?.is_empty() {
        return Err(error(
            "cannot stage a recovery graph with unavailable or corrupt artifacts",
        ));
    }
    stage_metadata_v2(path, graph)
}

fn stage_metadata_v2(
    path: &Path,
    graph: &RecoveryGraph,
) -> Result<StagedRecoveryManifest, RecoveryError> {
    graph.validate()?;
    let bytes = serde_json::to_vec_pretty(graph)
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
    Ok(StagedRecoveryManifest {
        inner: staged,
        graph: graph.clone(),
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
    stage_metadata_v2(path, &candidate)
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
) -> Result<StagedRecoveryManifest, RecoveryError> {
    graph.validate()?;
    let mut candidate = graph.clone();
    let surface = match candidate
        .public_surfaces
        .iter_mut()
        .find(|surface| surface.owner == owner)
    {
        Some(surface) => surface,
        None if expected_epoch == 0 => {
            candidate.public_surfaces.push(RecoveryPublicSurface {
                owner,
                declaration_root: None,
                epoch: 0,
                bindings: Vec::new(),
                source_instances: Vec::new(),
            });
            candidate.public_surfaces.last_mut().unwrap()
        }
        None => return Err(error("paired public surface is missing")),
    };
    if surface.epoch != expected_epoch {
        return Err(error("paired public visibility epoch changed"));
    }
    surface.epoch = expected_epoch
        .checked_add(1)
        .ok_or_else(|| error("public visibility epoch exhausted"))?;
    surface.bindings = bindings;
    surface.source_instances = source_instances;
    candidate.seal()?;
    stage_v2(path, recovery_root, &candidate)
}

fn high_water_candidate(
    graph: &RecoveryGraph,
    next: Generation,
) -> Result<RecoveryGraph, RecoveryError> {
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
    let mut candidate = graph.clone();
    candidate.high_water = next;
    candidate.seal()?;
    Ok(candidate)
}

/// Read only v2. Version 1 remains under the explicit legacy migration path;
/// future versions fail rather than being mistaken for a missing manifest.
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
            ))
        }
    };
    if bytes.len() > MAX_MANIFEST_BYTES {
        return Err(at(path, "recovery manifest exceeds the bounded size"));
    }
    let value: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|e| at(path, format!("invalid recovery manifest JSON: {e}")))?;
    let version = value
        .get("version")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| at(path, "recovery manifest has no integer version"))?;
    match version {
        1 => Ok(None),
        2 => {
            if value
                .get("public_schema")
                .and_then(serde_json::Value::as_str)
                != Some(PAIRED_PUBLIC_SCHEMA)
            {
                return Err(at(
                    path,
                    "v2 recovery graph lacks the supported paired public visibility schema",
                ));
            }
            if !value
                .get("public_surfaces")
                .is_some_and(serde_json::Value::is_array)
            {
                return Err(at(
                    path,
                    "v2 recovery graph lacks per-actor public surfaces",
                ));
            }
            let graph: RecoveryGraph = serde_json::from_value(value)
                .map_err(|e| at(path, format!("invalid v2 recovery graph: {e}")))?;
            graph.validate().map_err(|e| at(path, e.detail))?;
            let artifact_losses = graph
                .validate_artifact_files(recovery_root)
                .map_err(|e| at(path, e.detail))?;
            Ok(Some(RecoveryV2Read {
                graph,
                artifact_losses,
            }))
        }
        other => Err(at(
            path,
            format!("unsupported recovery manifest version {other}"),
        )),
    }
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
        self.artifacts.sort_by_key(RecoveryArtifactClosure::key);
        self.public_surfaces.sort_by(|a, b| a.owner.cmp(&b.owner));
        for surface in &mut self.public_surfaces {
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
        for node in &mut self.nodes {
            node.implementation_refs.sort();
            node.implementation_refs.dedup();
            node.artifact_refs.sort();
            node.artifact_refs.dedup();
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

    /// Heap values are never recovered. Compute loss from the final winning
    /// names, so an unavailable replacement cannot reveal an older binding.
    pub(crate) fn public_binding_tombstones(
        &self,
        owner: &RecoveryPublicOwner,
    ) -> Result<Vec<RecoveryLostPublicBinding>, RecoveryError> {
        self.validate()?;
        let surface = self
            .public_surfaces
            .iter()
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
        artifact_losses: &BTreeMap<String, Vec<RecoveryArtifactLoss>>,
    ) -> Result<BTreeMap<RecoverySymbolIdentity, RecoveryHead>, RecoveryError> {
        self.validate()?;
        let Some(root) = self
            .public_surfaces
            .iter()
            .find(|surface| &surface.owner == owner)
            .and_then(|surface| surface.declaration_root)
        else {
            return Ok(BTreeMap::new());
        };
        let by_id: BTreeMap<_, _> = self.nodes.iter().map(|node| (node.id, node)).collect();
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
        let mut availability = BTreeMap::new();
        for node in chain {
            let recoverability =
                node_recoverability(node.id, &by_id, artifact_losses, &mut availability);
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
        let by_id: BTreeMap<_, _> = self.nodes.iter().map(|node| (node.id, node)).collect();
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
    ) -> Result<BTreeMap<String, Vec<RecoveryArtifactLoss>>, RecoveryError> {
        self.validate()?;
        let root = fs::canonicalize(root)
            .map_err(|e| error(format!("could not resolve recovery root: {e}")))?;
        let mut losses = BTreeMap::new();
        for artifact in &self.artifacts {
            let (interface, product) = artifact.paths();
            validate_relative(interface)?;
            if let Some(product) = product {
                validate_relative(product)?;
            }
            let key = artifact.key();
            let mut item_losses = Vec::new();
            let verification = match artifact {
                RecoveryArtifactClosure::Home(reference) => {
                    verify_materialized_ref(&root, reference).map(|_| ())
                }
                RecoveryArtifactClosure::Join(reference) => {
                    verify_materialized_join(&root, reference).map(|_| ())
                }
            };
            if let Err(error) = verification {
                item_losses.push(artifact_error_loss(artifact, error));
            }
            if !item_losses.is_empty() {
                losses.insert(key, item_losses);
            }
        }
        Ok(losses)
    }
}

fn validate_shape(graph: &RecoveryGraph) -> Result<(), RecoveryError> {
    if graph.version != VERSION {
        return Err(error(format!(
            "unsupported recovery version {}",
            graph.version
        )));
    }
    if graph.public_schema != PAIRED_PUBLIC_SCHEMA {
        return Err(error("unsupported paired public visibility schema"));
    }
    let mut public_owners = BTreeSet::new();
    for surface in &graph.public_surfaces {
        if tidepool_repr::ActorPath::parse(&surface.owner.path).is_err()
            || surface.owner.incarnation == 0
            || !public_owners.insert(&surface.owner)
        {
            return Err(error("invalid or duplicate public actor owner"));
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
    if graph.lineage == 0 {
        return Err(error("recovery lineage must be nonzero"));
    }
    let mut nodes = BTreeMap::new();
    for node in &graph.nodes {
        if node.id.0 == 0 || node.id > graph.high_water || nodes.insert(node.id, node).is_some() {
            return Err(error(format!(
                "invalid or duplicate recovery node {}",
                node.id.0
            )));
        }
        if node.parent.is_some_and(|parent| parent >= node.id)
            || node.implementation_refs.iter().any(|dep| *dep >= node.id)
        {
            return Err(error(format!(
                "recovery node {} has a non-ancestor reference",
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
        for instance in &node.instances {
            if !valid_identity(&instance.dfun)
                || !valid_identity(&instance.class)
                || instance
                    .selected_axioms
                    .iter()
                    .any(|id| !valid_identity(id))
                || instance
                    .family_consistency_closure
                    .iter()
                    .any(|id| !valid_identity(id))
            {
                return Err(error(format!(
                    "recovery node {} has invalid instance evidence",
                    node.id.0
                )));
            }
        }
    }
    for node in &graph.nodes {
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
    for surface in &graph.public_surfaces {
        if surface
            .declaration_root
            .is_some_and(|root| !nodes.contains_key(&root))
        {
            return Err(error("recovery public root is missing"));
        }
        if surface
            .declaration_root
            .is_some_and(|root| root > graph.high_water)
        {
            return Err(error("recovery public root exceeds high-water"));
        }
    }

    let mut artifacts = BTreeMap::new();
    for artifact in &graph.artifacts {
        let (interface, product) = artifact.paths();
        if match artifact {
            RecoveryArtifactClosure::Home(reference) => {
                reference.unit.is_empty() || reference.module.is_empty()
            }
            RecoveryArtifactClosure::Join(reference) => {
                reference.unit.is_empty() || reference.module.is_empty()
            }
        } || validate_relative(interface).is_err()
            || product.is_some_and(|path| validate_relative(path).is_err())
        {
            return Err(error("invalid recovery artifact reference"));
        }
        if artifacts.insert(artifact.key(), artifact).is_some() {
            return Err(error("duplicate recovery artifact reference"));
        }
    }
    for node in &graph.nodes {
        if node
            .artifact_refs
            .iter()
            .any(|key| !artifacts.contains_key(key))
        {
            return Err(error(format!(
                "recovery node {} references a missing artifact",
                node.id.0
            )));
        }
        if matches!(&node.state, RecoveryNodeState::ExactArtifactClosure)
            && node.artifact_refs.is_empty()
            && !node.exports.is_empty()
        {
            return Err(error(format!(
                "recovery node {} has exports but no exact artifact closure",
                node.id.0
            )));
        }
    }
    Ok(())
}

fn checksum(graph: &RecoveryGraph) -> Result<String, RecoveryError> {
    let mut unsigned = graph.clone();
    unsigned.checksum.clear();
    let bytes = serde_json::to_vec(&unsigned)
        .map_err(|e| error(format!("could not encode recovery graph: {e}")))?;
    let mut domain = b"tidepool-recovery-graph-v2\0".to_vec();
    domain.extend_from_slice(&bytes);
    Ok(blake3::hash(&domain).to_hex().to_string())
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
) -> RecoveryArtifactLoss {
    let (component, path, kind) = match error {
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
    RecoveryArtifactLoss {
        component,
        path,
        kind,
    }
}

fn artifact_component(
    artifact: &RecoveryArtifactClosure,
    path: &Path,
) -> (RecoveryArtifactComponent, PathBuf) {
    let (interface, product) = artifact.paths();
    if product.is_some_and(|product| path.ends_with(product)) {
        (
            RecoveryArtifactComponent::Product,
            product.expect("matched product path").to_path_buf(),
        )
    } else {
        (
            RecoveryArtifactComponent::Interface,
            interface.to_path_buf(),
        )
    }
}

fn node_recoverability(
    id: Generation,
    nodes: &BTreeMap<Generation, &RecoveryNode>,
    artifact_losses: &BTreeMap<String, Vec<RecoveryArtifactLoss>>,
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
        RecoveryNodeState::LiveValueDependency { reason } => {
            Err(RecoveryLossReason::LiveValueDependency(reason.clone()))
        }
        RecoveryNodeState::MissingArtifactClosure { reason } => {
            Err(RecoveryLossReason::MissingArtifactClosure(reason.clone()))
        }
        RecoveryNodeState::ExactArtifactClosure => {
            if !node.live_dependencies.is_empty() {
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
                        if let Err(reason) =
                            node_recoverability(dependency, nodes, artifact_losses, memo)
                        {
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
    }
}
fn at(path: &Path, detail: impl Into<String>) -> RecoveryError {
    RecoveryError {
        path: Some(path.to_path_buf()),
        detail: detail.into(),
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
    use tidepool_repr::execution_schema::{CachedHomeOwner, ModuleVersion};
    use tidepool_toolchain::recovery_artifacts::{
        materialize_recovery_closure, RecoveryArtifactInput,
    };

    const IFACE_SHA: [u8; 32] = [
        0xc7, 0x96, 0xd3, 0x7c, 0x6d, 0x49, 0x1e, 0x8f, 0x0c, 0x6e, 0x9b, 0x83, 0xee, 0xd3, 0x4c,
        0x15, 0xc0, 0xf3, 0x77, 0xf9, 0xf0, 0xf3, 0xcb, 0xb3, 0x21, 0x6f, 0xbb, 0xf7, 0x76, 0xda,
        0x63, 0x25,
    ];
    const PRODUCT_SHA: [u8; 32] = [
        0xa8, 0x79, 0x21, 0x57, 0xcb, 0x4f, 0x27, 0xfb, 0x94, 0x9c, 0x03, 0x5f, 0x45, 0x51, 0x8c,
        0x61, 0xe8, 0x84, 0xbb, 0x86, 0xe6, 0xf4, 0x20, 0x20, 0x43, 0x79, 0xc2, 0xba, 0xa8, 0xbe,
        0xb6, 0x6e,
    ];

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

    fn fixture(root: &Path) -> RecoveryGraph {
        let source = tempfile::tempdir().unwrap();
        let iface = source.path().join("Lib.hi");
        let product = source.path().join("Lib.product");
        fs::write(&iface, b"interface").unwrap();
        fs::write(&product, b"product").unwrap();
        let home_owner = CachedHomeOwner {
            unit: "main".into(),
            module: "Lib".into(),
            module_version: ModuleVersion([0x22; 32]),
            skinny_iface_sha256: IFACE_SHA,
            product_sha256: PRODUCT_SHA,
        };
        let materialized = materialize_recovery_closure(
            root,
            [0x11; 32],
            &[RecoveryArtifactInput {
                owner: &home_owner,
                interface_source: &iface,
                product_source: &product,
            }],
        )
        .unwrap();
        let artifact = RecoveryArtifactClosure::Home(materialized.into_iter().next().unwrap());
        let key = artifact.key();
        let export = RecoveryExport {
            identity: identity("answer"),
            kind: RecoveryExportKind::Value,
            children: vec![],
        };
        let mut graph = RecoveryGraph {
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
            }],
            nodes: vec![
                RecoveryNode {
                    id: Generation(1),
                    parent: None,
                    kind: RecoveryNodeKind::Authored,
                    implementation_refs: vec![],
                    artifact_refs: vec![key],
                    exports: vec![export.clone()],
                    retracts: vec![],
                    workbench_imports: vec!["qualified Data.Map.Strict as Map".into()],
                    instances: vec![],
                    live_dependencies: vec![],
                    state: RecoveryNodeState::ExactArtifactClosure,
                },
                RecoveryNode {
                    id: Generation(2),
                    parent: Some(Generation(1)),
                    kind: RecoveryNodeKind::Join,
                    implementation_refs: vec![Generation(1)],
                    artifact_refs: vec![],
                    exports: vec![export],
                    retracts: vec![],
                    workbench_imports: vec!["Data.Proxy (Proxy (..))".into()],
                    instances: vec![],
                    live_dependencies: vec![],
                    state: RecoveryNodeState::MissingArtifactClosure {
                        reason: "winner artifact unavailable".into(),
                    },
                },
            ],
            artifacts: vec![artifact],
            checksum: String::new(),
        };
        graph.seal().unwrap();
        graph
    }

    #[test]
    fn binding_only_publication_is_durable_and_restarts_as_a_winner_tombstone() {
        let dir = tempfile::tempdir().unwrap();
        let manifest = dir.path().join("declarations.json");
        let original = RecoveryGraph::empty(41, 99).unwrap();
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
            &original,
            root.clone(),
            0,
            vec![first],
            vec![],
        )
        .unwrap();
        assert!(!manifest.exists(), "staging is not public authority");
        let published = match staged.publish() {
            RecoveryPublishOutcome::Durable { graph, .. } => graph,
            _ => panic!("expected durable publication"),
        };
        assert_eq!(published.public_surfaces[0].declaration_root, None);
        assert_eq!(published.high_water, Generation(0));
        assert_eq!(published.public_surfaces[0].epoch, 1);
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
            vec![next],
            vec![],
        )
        .unwrap();
        let replacement = match staged.publish() {
            RecoveryPublishOutcome::Durable { graph, .. } => graph,
            _ => panic!("expected durable replacement"),
        };
        let restarted = read_v2(&manifest, dir.path()).unwrap().unwrap().graph;
        assert_eq!(restarted, replacement);
        assert_eq!(
            restarted.public_binding_tombstones(&root).unwrap(),
            vec![RecoveryLostPublicBinding {
                name: "answer".into(),
                winner: RecoveryBindingId {
                    session: 41,
                    variable: 2,
                },
            }]
        );
        assert!(stage_public_visibility_v2(
            &manifest,
            dir.path(),
            &replacement,
            root,
            1,
            vec![],
            vec![],
        )
        .is_err());
    }

    #[test]
    fn same_session_actor_surfaces_keep_independent_winners_and_epochs() {
        let dir = tempfile::tempdir().unwrap();
        let manifest = dir.path().join("declarations.json");
        let parent = owner("root");
        let child = owner("root/child");
        let initial = RecoveryGraph::empty(41, 99).unwrap();
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
            &initial,
            parent.clone(),
            0,
            vec![binding(1)],
            vec![],
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
        )
        .is_err());
        let restored = read_v2(&manifest, dir.path()).unwrap().unwrap().graph;
        assert_eq!(restored.public_surfaces.len(), 2);
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
                .public_surfaces
                .iter()
                .map(|surface| surface.epoch)
                .collect::<Vec<_>>(),
            vec![1, 1]
        );
    }

    #[test]
    fn sibling_actors_project_their_own_declaration_roots() {
        let dir = tempfile::tempdir().unwrap();
        let mut graph = fixture(dir.path());
        graph.public_surfaces[0].declaration_root = Some(Generation(1));
        graph.public_surfaces.push(RecoveryPublicSurface {
            owner: owner("root/child"),
            declaration_root: Some(Generation(2)),
            epoch: 0,
            bindings: vec![],
            source_instances: vec![],
        });
        graph.seal().unwrap();
        let parent = graph.projection(&owner("root"), &BTreeMap::new()).unwrap();
        let child = graph
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
        let mut graph = RecoveryGraph::empty(41, 99).unwrap();
        let surface = RecoveryPublicSurface {
            owner: owner("root"),
            declaration_root: None,
            epoch: 0,
            bindings: vec![],
            source_instances: vec![],
        };
        graph.public_surfaces = vec![surface.clone(), surface];
        assert!(graph.seal().is_err());
        graph.public_surfaces.pop();
        graph.public_surfaces[0].owner.path = "Root".into();
        assert!(graph.seal().is_err());
    }

    #[test]
    fn legacy_v2_without_paired_public_schema_is_explicitly_refused() {
        let dir = tempfile::tempdir().unwrap();
        let manifest = dir.path().join("declarations.json");
        fs::write(&manifest, br#"{"version":2,"source_session":41}"#).unwrap();
        let error = read_v2(&manifest, dir.path()).err().unwrap();
        assert!(error
            .detail
            .contains("lacks the supported paired public visibility schema"));
    }

    #[test]
    fn checksum_covers_the_graph_and_projection_preserves_lost_winner_tombstones() {
        let dir = tempfile::tempdir().unwrap();
        let graph = fixture(dir.path());
        let bytes = serde_json::to_vec(&graph).unwrap();
        let decoded: RecoveryGraph = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(decoded, graph);
        decoded.validate().unwrap();
        graph.validate_artifact_files(dir.path()).unwrap();
        let projected = graph.projection(&owner("root"), &BTreeMap::new()).unwrap();
        assert!(
            matches!(projected.get(&identity("answer")), Some(RecoveryHead::Tombstone(t)) if t.winner == Generation(2))
        );

        let mut changed = graph;
        changed.nodes[1].state = RecoveryNodeState::ExactArtifactClosure;
        assert!(changed.validate().is_err());
    }

    #[test]
    fn artifact_paths_must_stay_relative_to_the_recovery_root() {
        let dir = tempfile::tempdir().unwrap();
        let mut graph = fixture(dir.path());
        match &mut graph.artifacts[0] {
            RecoveryArtifactClosure::Home(reference) => {
                reference.interface_path = PathBuf::from("../escape.hi");
            }
            RecoveryArtifactClosure::Join(_) => unreachable!(),
        }
        assert!(graph.seal().unwrap_err().detail.contains("artifact"));
    }

    #[test]
    fn reservations_advance_exactly_one_generation_without_publishing_a_node() {
        let dir = tempfile::tempdir().unwrap();
        let graph = fixture(dir.path());
        let product = match &graph.artifacts[0] {
            RecoveryArtifactClosure::Home(reference) => reference.product_path.clone(),
            RecoveryArtifactClosure::Join(_) => unreachable!(),
        };
        fs::remove_file(dir.path().join(product)).unwrap();
        let candidate = high_water_candidate(&graph, Generation(3)).unwrap();
        assert_eq!(candidate.high_water, Generation(3));
        assert_eq!(candidate.public_surfaces, graph.public_surfaces);
        assert_eq!(candidate.nodes, graph.nodes);
        assert!(high_water_candidate(&graph, Generation(4)).is_err());
    }

    #[test]
    fn staged_high_water_is_invisible_until_publish_and_survives_readback() {
        let dir = tempfile::tempdir().unwrap();
        let graph = fixture(dir.path());
        let manifest_path = dir.path().join("recovery.json");
        fs::write(&manifest_path, b"previous manifest").unwrap();

        let staged = stage_high_water_v2(&manifest_path, &graph, Generation(3)).unwrap();
        assert_eq!(staged.candidate_graph().high_water, Generation(3));
        assert_eq!(fs::read(&manifest_path).unwrap(), b"previous manifest");
        drop(staged);
        assert_eq!(fs::read(&manifest_path).unwrap(), b"previous manifest");

        match stage_high_water_v2(&manifest_path, &graph, Generation(3))
            .unwrap()
            .publish()
        {
            RecoveryPublishOutcome::Durable {
                graph: published,
                publication,
            } => {
                assert_eq!(publication.path(), manifest_path);
                assert_eq!(published.high_water, Generation(3));
                assert_eq!(published.public_surfaces, graph.public_surfaces);
                assert_eq!(published.nodes, graph.nodes);
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
        assert_eq!(restored.graph.high_water, Generation(3));
        assert_eq!(restored.graph.public_surfaces, graph.public_surfaces);
        assert!(restored.artifact_losses.is_empty());
    }

    #[test]
    fn pre_rename_failure_keeps_target_unpublished() {
        let dir = tempfile::tempdir().unwrap();
        let graph = fixture(dir.path());
        let manifest_path = dir.path().join("recovery.json");
        let staged = stage_high_water_v2(&manifest_path, &graph, Generation(3)).unwrap();
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
        let graph = fixture(dir.path());
        let product = match &graph.artifacts[0] {
            RecoveryArtifactClosure::Home(reference) => reference.product_path.clone(),
            RecoveryArtifactClosure::Join(_) => unreachable!(),
        };
        fs::write(dir.path().join(product), b"changed").unwrap();
        let losses = graph.validate_artifact_files(dir.path()).unwrap();
        assert!(losses
            .values()
            .flatten()
            .any(|loss| matches!(&loss.kind, RecoveryArtifactLossKind::DigestMismatch)));
    }

    #[test]
    fn a_missing_winning_artifact_becomes_a_tombstone_without_resurrection() {
        let dir = tempfile::tempdir().unwrap();
        let mut graph = fixture(dir.path());
        graph.nodes[1].state = RecoveryNodeState::ExactArtifactClosure;
        graph.nodes[1].artifact_refs = graph.nodes[0].artifact_refs.clone();
        graph.seal().unwrap();
        let product = match &graph.artifacts[0] {
            RecoveryArtifactClosure::Home(reference) => reference.product_path.clone(),
            RecoveryArtifactClosure::Join(_) => unreachable!(),
        };
        fs::remove_file(dir.path().join(product)).unwrap();
        let losses = graph.validate_artifact_files(dir.path()).unwrap();
        let projected = graph.projection(&owner("root"), &losses).unwrap();
        assert!(
            matches!(projected.get(&identity("answer")), Some(RecoveryHead::Tombstone(t)) if t.winner == Generation(2))
        );
    }

    #[test]
    fn restart_read_keeps_missing_winning_artifact_as_tombstone() {
        let dir = tempfile::tempdir().unwrap();
        let mut graph = fixture(dir.path());
        graph.nodes[1].state = RecoveryNodeState::ExactArtifactClosure;
        graph.nodes[1].artifact_refs = graph.nodes[0].artifact_refs.clone();
        graph.seal().unwrap();
        let manifest_path = dir.path().join("recovery.json");
        match stage_v2(&manifest_path, dir.path(), &graph)
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
        let product = match &graph.artifacts[0] {
            RecoveryArtifactClosure::Home(reference) => reference.product_path.clone(),
            RecoveryArtifactClosure::Join(_) => unreachable!(),
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
        let dir = tempfile::tempdir().unwrap();
        let mut graph = fixture(dir.path());
        let old = identity_in("G1", "foo");
        let replacement = identity_in("G2", "foo");
        graph.nodes[0].exports[0].identity = old.clone();
        graph.nodes[1].exports[0].identity = replacement.clone();
        graph.nodes[1].retracts = vec![old.clone()];
        graph.seal().unwrap();

        let projected = graph.projection(&owner("root"), &BTreeMap::new()).unwrap();
        assert!(!projected.contains_key(&old));
        assert!(matches!(
            projected.get(&replacement),
            Some(RecoveryHead::Tombstone(t)) if t.winner == Generation(2)
        ));
    }

    #[test]
    fn workbench_imports_fold_in_lexical_order_and_keep_exact_specs() {
        let dir = tempfile::tempdir().unwrap();
        let mut graph = fixture(dir.path());
        graph.nodes[1]
            .workbench_imports
            .push("qualified Data.Map.Strict as Map".into());
        graph.seal().unwrap();
        assert_eq!(
            graph.workbench_imports(Generation(2)).unwrap(),
            [
                "qualified Data.Map.Strict as Map",
                "Data.Proxy (Proxy (..))"
            ]
        );
    }
}
