//! Immutable run recovery snapshots. SQLite selects roots; compiler and
//! declaration facts live only in authenticated DAG-CBOR blocks and HAMTs.
use crate::session::recovery_store::{
    RecoveryDomainId, RecoveryDomainState, RunRecoveryStore, StoreDisposition, StoreError,
    StoreRevision,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use tidepool_repr::Generation;
use tidepool_toolchain::artifact_inventory::{ArtifactDescriptor, ArtifactId};
use tidepool_toolchain::declaration_join::ExactModuleIdentity;
use tidepool_toolchain::exact_context::{ContextRoot, SnapshotError};
use tidepool_toolchain::recovery_artifacts::{
    RecoveryArtifactRef, RecoveryJoinRef, RecoveryModuleInterfaceRef, RecoveryValueInterfaceRef,
};

#[path = "newrecovery_v2/snapshots.rs"]
mod snapshots;
#[cfg(test)]
pub(crate) use snapshots::{observe_context_materialization, ContextMaterializationWork};
pub(crate) use snapshots::{
    PreparedCompilerContext, RecoveryDomainReader, RecoveryGraph, RecoveryPublicSurface,
    ScopedCompilerContext, StagedCompilerContextPublication, StagedRecoveryManifest,
};

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryPublicOwner {
    path: String,
    incarnation: u64,
    notebook_generation: crate::session::NotebookGenerationId,
}

impl RecoveryPublicOwner {
    pub fn notebook_generation(&self) -> crate::session::NotebookGenerationId {
        self.notebook_generation
    }
    pub fn for_notebook_generation(
        &self,
        generation: crate::session::NotebookGenerationId,
    ) -> Self {
        Self {
            notebook_generation: generation,
            ..self.clone()
        }
    }
    /// Canonical actor identity, separate from its selected notebook generation.
    pub fn actor_owner(&self) -> Self {
        self.for_notebook_generation(crate::session::NotebookGenerationId::INITIAL)
    }
    pub fn new(path: &tidepool_repr::ActorPath, incarnation: u64) -> Option<Self> {
        if incarnation == 0 {
            return None;
        }
        Some(Self {
            path: path.to_string(),
            incarnation,
            notebook_generation: crate::session::NotebookGenerationId::INITIAL,
        })
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

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
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
    #[serde(with = "option_generation_serde")]
    pub parent: Option<Generation>,
    pub kind: RecoveryNodeKind,
    #[serde(with = "generation_vec_serde")]
    pub implementation_refs: Vec<Generation>,
    pub context_root: ContextRoot,
    pub lexical_roots: Vec<ExactModuleIdentity>,
    /// This declaration's producer artifacts, not its cumulative context.
    pub artifact_refs: Vec<ArtifactId>,
    pub exports: Vec<RecoveryExport>,
    pub retracts: Vec<RecoverySymbolIdentity>,
    pub workbench_imports: Vec<String>,
    pub instances: RecoveryInstanceInventory,
    pub live_dependencies: Vec<RecoveryLiveDependency>,
    pub state: RecoveryNodeState,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryNodeKind {
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
pub enum RecoveryArtifactClosure {
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
pub struct RecoverySymbolIdentity {
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
pub struct RecoveryExport {
    pub identity: RecoverySymbolIdentity,
    pub kind: RecoveryExportKind,
    /// Exact GHC identities of constructors, selectors, methods, or associated
    /// type members visible through this head.
    pub children: Vec<RecoverySymbolIdentity>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryExportKind {
    Value,
    Type,
    Class,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryInstanceEvidence {
    pub dfun: RecoverySymbolIdentity,
    pub class: RecoverySymbolIdentity,
    pub selected: bool,
    pub selected_axioms: Vec<RecoverySymbolIdentity>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryInstanceInventory {
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

impl tidepool_toolchain::exact_context::SnapshotKey for RecoveryPublicSourceInstance {}

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

#[derive(Debug)]
pub(crate) struct RecoveryError {
    pub path: Option<PathBuf>,
    pub detail: String,
    pub kind: RecoveryErrorKind,
}
#[derive(Debug)]
pub(crate) enum RecoveryErrorKind {
    Manifest,
    Snapshot(SnapshotError),
    Store(StoreError),
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecoveryRefusal {
    UnsupportedOldFormat { version: u64 },
    UnsupportedFutureFormat { version: u64 },
}
impl std::fmt::Display for RecoveryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.detail)
    }
}
impl std::error::Error for RecoveryError {}
impl From<SnapshotError> for RecoveryError {
    fn from(error: SnapshotError) -> Self {
        Self {
            path: None,
            detail: error.to_string(),
            kind: RecoveryErrorKind::Snapshot(error),
        }
    }
}
impl From<StoreError> for RecoveryError {
    fn from(error: StoreError) -> Self {
        Self {
            path: Some(error.path.clone()),
            detail: error.to_string(),
            kind: RecoveryErrorKind::Store(error),
        }
    }
}
fn error(detail: impl Into<String>) -> RecoveryError {
    RecoveryError {
        path: None,
        detail: detail.into(),
        kind: RecoveryErrorKind::Manifest,
    }
}

pub(crate) fn load_domain_graph(
    store: Arc<RunRecoveryStore>,
    domain: RecoveryDomainId,
) -> Result<RecoveryGraph, RecoveryError> {
    let state = store
        .read_domain(domain)?
        .ok_or_else(|| error("recovery domain is absent"))?;
    RecoveryGraph::hydrate_domain(store, domain, &state)
}

/// Resolve only payloads introduced by the actual selected-context successor.
/// Authenticated retained references remain the run's immutable custody facts.
pub(crate) fn materialize_missing_context_artifacts(
    graph: &RecoveryGraph,
    artifact_root: &Path,
    context: &tidepool_toolchain::declaration_join::ExactDeclarationContext,
    previous: Option<&ContextRoot>,
) -> Result<Vec<RecoveryArtifactClosure>, crate::session::SessionError> {
    let base = previous
        .map(|root| graph.context(root))
        .transpose()
        .map_err(|error| crate::session::recovery::graph_error(graph.store().path(), error))?;
    let snapshot = context.snapshot();
    let mut introduced = snapshot
        .selection()
        .artifact_ids_introduced(
            base.as_ref()
                .map(|base| base.snapshot().selection().as_ref()),
        )
        .map_err(|error| crate::session::SessionError::RecoverySnapshot(Box::new(error)))?;
    introduced.retain(|id| !graph.contains_artifact(*id));
    let (products, interfaces) = context
        .materialize_recovery_products_and_interfaces_for_artifacts(artifact_root, &introduced)?;
    let (joins, values) = context
        .materialize_joined_and_value_interfaces_for_artifacts(artifact_root, &introduced)?;
    Ok(products
        .into_iter()
        .map(RecoveryArtifactClosure::Home)
        .chain(
            interfaces
                .into_iter()
                .map(RecoveryArtifactClosure::ModuleInterface),
        )
        .chain(joins.into_iter().map(RecoveryArtifactClosure::Join))
        .chain(
            values
                .into_iter()
                .map(RecoveryArtifactClosure::ValueInterface),
        )
        .filter(|artifact| !graph.contains_artifact(artifact.artifact_id()))
        .collect())
}

pub(crate) fn load_or_create_domain_graph(
    store: Arc<RunRecoveryStore>,
    domain: RecoveryDomainId,
    source_session: u64,
    initial_ceiling: Generation,
) -> Result<(RecoveryGraph, RecoveryDomainState, bool), crate::session::SessionError> {
    let convert = |error| crate::session::recovery::graph_error(store.path(), error);
    if let Some(state) = store
        .read_domain(domain)
        .map_err(|e| crate::session::SessionError::RecoveryStore(Box::new(e)))?
    {
        if !matches!(domain, RecoveryDomainId::Root) {
            return Err(convert(error("independent recovery domain already exists")));
        }
        let graph =
            RecoveryGraph::hydrate_domain(store.clone(), domain, &state).map_err(convert)?;
        return Ok((graph, state, true));
    }
    if matches!(domain,RecoveryDomainId::Session(session) if session.0!=source_session) {
        return Err(convert(error(
            "independent recovery domain differs from issuing session",
        )));
    }
    let graph = RecoveryGraph::empty(store.clone(), domain, source_session);
    let (mut graph, root, blocks) = graph.materialize_domain().map_err(convert)?;
    let state = match store.create_domain(domain, source_session, initial_ceiling, root, blocks) {
        StoreDisposition::Committed(state) => state,
        StoreDisposition::Refused(reason) => {
            return Err(convert(error(format!(
                "domain creation refused: {reason:?}"
            ))));
        }
        StoreDisposition::RecoveryRequired(error) => {
            return Err(crate::session::SessionError::RecoveryStore(Box::new(error)));
        }
    };
    graph.install_store_state(&state);
    Ok((graph, state, false))
}

impl RecoveryGraph {
    pub(crate) fn public_binding_tombstones(
        &self,
        owner: &RecoveryPublicOwner,
    ) -> Result<Vec<RecoveryLostPublicBinding>, RecoveryError> {
        let Some(surface) = self.surface(owner)? else {
            return Ok(Vec::new());
        };
        Ok(surface
            .bindings
            .iter()
            .map(|(name, winner)| RecoveryLostPublicBinding {
                name: name.clone(),
                winner: *winner,
            })
            .collect())
    }
    pub(crate) fn projection(
        &self,
        owner: &RecoveryPublicOwner,
        losses: &BTreeMap<ArtifactId, Vec<RecoveryArtifactLoss>>,
    ) -> Result<BTreeMap<RecoverySymbolIdentity, RecoveryHead>, RecoveryError> {
        self.project_heads(owner, losses, RecoveryAvailability::Native)
    }
    pub(crate) fn interface_projection(
        &self,
        owner: &RecoveryPublicOwner,
        losses: &BTreeMap<ArtifactId, Vec<RecoveryArtifactLoss>>,
    ) -> Result<BTreeMap<RecoverySymbolIdentity, RecoveryHead>, RecoveryError> {
        self.project_heads(owner, losses, RecoveryAvailability::Interface)
    }
    fn ancestry(&self, root: Generation) -> Result<Vec<Arc<RecoveryNode>>, RecoveryError> {
        let mut chain = Vec::new();
        let mut cursor = Some(root);
        while let Some(id) = cursor {
            let node = self
                .node(id)?
                .ok_or_else(|| error("declaration ancestry is incomplete"))?;
            if node.parent.is_some_and(|parent| parent >= id) {
                return Err(error("declaration ancestry is not strictly causal"));
            }
            cursor = node.parent;
            chain.push(node);
        }
        chain.reverse();
        Ok(chain)
    }
    fn project_heads(
        &self,
        owner: &RecoveryPublicOwner,
        losses: &BTreeMap<ArtifactId, Vec<RecoveryArtifactLoss>>,
        availability: RecoveryAvailability,
    ) -> Result<BTreeMap<RecoverySymbolIdentity, RecoveryHead>, RecoveryError> {
        let Some(root) = self
            .surface(owner)?
            .and_then(|surface| surface.declaration_root)
        else {
            return Ok(BTreeMap::new());
        };
        let mut visible = BTreeMap::new();
        let mut memo = BTreeMap::new();
        for node in self.ancestry(root)? {
            let recoverability =
                self.node_recoverability(node.id, losses, availability, &mut memo)?;
            for identity in &node.retracts {
                visible.remove(identity);
            }
            for export in &node.exports {
                let head = match &recoverability {
                    Ok(()) => RecoveryHead::Available {
                        winner: node.id,
                        export: export.clone(),
                    },
                    Err(reason) => RecoveryHead::Tombstone(RecoveryTombstone {
                        identity: export.identity.clone(),
                        winner: node.id,
                        reason: reason.clone(),
                    }),
                };
                visible.insert(export.identity.clone(), head);
            }
        }
        Ok(visible)
    }
    fn node_recoverability(
        &self,
        id: Generation,
        losses: &BTreeMap<ArtifactId, Vec<RecoveryArtifactLoss>>,
        availability: RecoveryAvailability,
        memo: &mut BTreeMap<Generation, Result<(), RecoveryLossReason>>,
    ) -> Result<Result<(), RecoveryLossReason>, RecoveryError> {
        if let Some(result) = memo.get(&id) {
            return Ok(result.clone());
        }
        let Some(node) = self.node(id)? else {
            return Ok(Err(RecoveryLossReason::MissingArtifactClosure(
                "referenced declaration is absent".into(),
            )));
        };
        let result = match &node.state {
            RecoveryNodeState::MissingArtifactClosure { reason } => {
                Err(RecoveryLossReason::MissingArtifactClosure(reason.clone()))
            }
            RecoveryNodeState::LiveValueDependency { reason }
                if availability == RecoveryAvailability::Native =>
            {
                Err(RecoveryLossReason::LiveValueDependency(reason.clone()))
            }
            _ if availability == RecoveryAvailability::Native
                && !node.live_dependencies.is_empty() =>
            {
                Err(RecoveryLossReason::LiveValueDependency(format!(
                    "{} live dependency records",
                    node.live_dependencies.len()
                )))
            }
            _ => {
                let mut failure = node.artifact_refs.iter().find_map(|id| {
                    losses
                        .get(id)
                        .and_then(|items| items.first())
                        .cloned()
                        .map(RecoveryLossReason::Artifact)
                });
                if failure.is_none() {
                    for dependency in node
                        .parent
                        .into_iter()
                        .chain(node.implementation_refs.iter().copied())
                    {
                        if dependency >= id {
                            return Err(error("implementation dependency is not strictly causal"));
                        }
                        if let Err(reason) =
                            self.node_recoverability(dependency, losses, availability, memo)?
                        {
                            failure = Some(RecoveryLossReason::Dependency {
                                generation: dependency,
                                detail: format!("{reason:?}"),
                            });
                            break;
                        }
                    }
                }
                failure.map_or(Ok(()), Err)
            }
        };
        memo.insert(id, result.clone());
        Ok(result)
    }
    pub(crate) fn workbench_imports(&self, root: Generation) -> Result<Vec<String>, RecoveryError> {
        let mut imports = Vec::new();
        for node in self.ancestry(root)? {
            for spec in &node.workbench_imports {
                if !imports.contains(spec) {
                    imports.push(spec.clone())
                }
            }
        }
        Ok(imports)
    }
}
fn relative(path: &Path) -> bool {
    !path.as_os_str().is_empty() && path.components().all(|c| matches!(c, Component::Normal(_)))
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
