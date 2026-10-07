//! Exact declaration inputs retain original products independently of source
//! lookup, while their explicit virtual graph owns lexical visibility.

use ciborium::value::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::artifact_inventory::{
    admission_failure, ArtifactEntry, ArtifactId, ArtifactInventory, ArtifactInventoryFailure,
    ArtifactMetadataSnapshot, ArtifactPayload, ArtifactView, JoinedInterfaceRole, NativeGroupKey,
};
use crate::certified_products::{
    certify_selected_owned_products_in_context_with_validation, PendingCertifiedGroup,
};
use crate::declaration_join::{
    AcceptedJoin, CertifiedAuthoredDeclaration, DeclarationArtifact, ExactIfaceArtifact,
    ExactInterfaceOwner, ExactLexicalNode, ExactModuleIdentity, ModuleSnapshot,
};
use crate::recovery_artifacts::{
    self, CertifiedJoinedInterface, CertifiedRecoveryProduct, CertifiedValueInterface,
    MaterializationMode, PackageInterfaceValidation, RecoveryArtifactRef, RecoveryJoinRef,
    RecoveryValueInterfaceRef,
};
use crate::CompileError;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExactDeclarationContext {
    producer: [u8; 32],
    inventory: ArtifactView,
    lexical: Vec<ExactLexicalNode>,
    template_imports: Option<Arc<RetainedTemplateImports>>,
    original_instance_environment: OriginalInstanceEnvironment,
}

/// Only the original compiler output proof can establish complete instance
/// visibility. Type projections and generic declaration contexts have none.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum OriginalInstanceEnvironment {
    #[default]
    Unknown,
    Complete {
        target: ExactModuleIdentity,
    },
    MissingOriginalOwners(Vec<ExactModuleIdentity>),
}

/// Trusted source recipe associated with request-local native type custody.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RequestHelperRecipe {
    #[default]
    None,
    ActorReply,
}

impl RequestHelperRecipe {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::ActorReply => "actor-reply",
        }
    }
}

pub(crate) fn encode_request_authorization(
    signatures: &crate::checked_cell::RequestTypeSignatures,
    recipe: RequestHelperRecipe,
    purpose: Option<Value>,
) -> Value {
    Value::Array(vec![
        text("request-types2"),
        signatures.authorization_value(),
        text(recipe.as_str()),
        purpose.unwrap_or(Value::Null),
    ])
}

/// Request-local signatures together with their trusted helper recipe.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequestAnnotations {
    signatures: Arc<crate::checked_cell::RequestTypeSignatures>,
    helper_recipe: RequestHelperRecipe,
}

impl RequestAnnotations {
    fn new(
        signatures: Arc<crate::checked_cell::RequestTypeSignatures>,
        helper_recipe: RequestHelperRecipe,
    ) -> Self {
        Self {
            signatures,
            helper_recipe,
        }
    }

    pub fn signatures(&self) -> &Arc<crate::checked_cell::RequestTypeSignatures> {
        &self.signatures
    }

    pub fn helper_recipe(&self) -> RequestHelperRecipe {
        self.helper_recipe
    }
}

/// Request-local compiler inputs. Persistent publication retains declarations
/// separately, so native request annotations cannot enter a lexical snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExactCompileContext {
    declarations: Arc<ExactDeclarationContext>,
    request_annotations: Option<RequestAnnotations>,
}

impl ExactCompileContext {
    pub fn new(declarations: Arc<ExactDeclarationContext>) -> Self {
        Self {
            declarations,
            request_annotations: None,
        }
    }

    pub fn declarations(&self) -> &Arc<ExactDeclarationContext> {
        &self.declarations
    }

    pub fn request_types(&self) -> Option<&Arc<crate::checked_cell::RequestTypeSignatures>> {
        self.request_annotations
            .as_ref()
            .map(RequestAnnotations::signatures)
    }

    pub fn request_annotations(&self) -> Option<&RequestAnnotations> {
        self.request_annotations.as_ref()
    }

    /// Replace signature authentication while preserving the chosen helper mode.
    pub fn with_request_types(
        mut self,
        signatures: Arc<crate::checked_cell::RequestTypeSignatures>,
    ) -> Self {
        self.request_annotations = Some(RequestAnnotations::new(
            signatures,
            self.request_helper_recipe(),
        ));
        self
    }

    pub fn with_request_helper_recipe(
        mut self,
        recipe: RequestHelperRecipe,
    ) -> Result<Self, CompileError> {
        match &mut self.request_annotations {
            Some(annotations) => annotations.helper_recipe = recipe,
            None if recipe == RequestHelperRecipe::None => {}
            None => {
                return Err(CompileError::ExtractFailed(
                    "actor reply helpers require original request type signatures".into(),
                ));
            }
        }
        Ok(self)
    }

    pub fn request_helper_recipe(&self) -> RequestHelperRecipe {
        self.request_annotations
            .as_ref()
            .map_or(RequestHelperRecipe::None, RequestAnnotations::helper_recipe)
    }

    pub(crate) fn with_declarations(mut self, declarations: Arc<ExactDeclarationContext>) -> Self {
        self.declarations = declarations;
        self
    }

    fn authorization(&self, purpose: Option<Value>) -> Option<Value> {
        match &self.request_annotations {
            Some(annotations) => Some(encode_request_authorization(
                &annotations.signatures,
                annotations.helper_recipe,
                purpose,
            )),
            None => purpose,
        }
    }

    pub(crate) fn prepare_compilation(
        &self,
        root: &Path,
        producer: &[u8],
    ) -> Result<ExactCompilationRequest, CompileError> {
        self.prepare_compilation_with_authorization(root, producer, None)
    }

    pub(crate) fn prepare_compilation_with_authorization(
        &self,
        root: &Path,
        producer: &[u8],
        authorization: Option<Value>,
    ) -> Result<ExactCompilationRequest, CompileError> {
        self.declarations.prepare_compilation_with_authorization(
            root,
            producer,
            self.authorization(authorization),
        )
    }

    pub(crate) fn prepare_compilation_authorizing(
        &self,
        root: &Path,
        producer: &[u8],
        authorize: impl FnOnce([u8; 32]) -> Result<Value, CompileError>,
    ) -> Result<ExactCompilationRequest, CompileError> {
        self.declarations
            .prepare_compilation_authorizing(root, producer, |semantic| {
                Ok(self
                    .authorization(Some(authorize(semantic)?))
                    .expect("purpose authorization"))
            })
    }
}

impl From<Arc<ExactDeclarationContext>> for ExactCompileContext {
    fn from(declarations: Arc<ExactDeclarationContext>) -> Self {
        Self::new(declarations)
    }
}

const EXACT_SCOPE_BYTES_LIMIT: usize = 4 << 20;
const EXACT_SCOPE_GRAPHS_LIMIT: usize = 4096;

fn validate_execution_graph_budget<'a>(
    graphs: impl IntoIterator<Item = &'a crate::execution_source::CertifiedExecutionSourceGraph>,
) -> Result<(), CompileError> {
    let fits = graphs
        .into_iter()
        .try_fold((0usize, 0usize), |(count, bytes), graph| {
            let count = count.checked_add(1)?;
            let bytes = bytes.checked_add(graph.bytes().len())?;
            (count <= EXACT_SCOPE_GRAPHS_LIMIT
                && bytes <= crate::execution_source::GRAPH_BYTES_LIMIT)
                .then_some((count, bytes))
        })
        .is_some();
    if fits {
        Ok(())
    } else {
        Err(failure(
            "original execution graphs exceed their 64 MiB or 4096 graph bound",
        ))
    }
}

fn canonical_source_interface(
    entry: &crate::artifact_inventory::ArtifactEntry,
) -> Option<&crate::certified_products::CertifiedModuleInterface> {
    match &entry.payload {
        ArtifactPayload::Canonical(interface) => Some(interface),
        ArtifactPayload::Original(product) => product.module_interface(),
        ArtifactPayload::Interface(_, _) => None,
    }
}

#[cfg(test)]
fn original_products(entries: &[Arc<ArtifactEntry>]) -> Vec<&CertifiedRecoveryProduct> {
    entries
        .iter()
        .filter_map(|entry| match &entry.payload {
            ArtifactPayload::Original(product) => Some(product),
            _ => None,
        })
        .collect()
}

fn original_products_by_id(
    entries: &[Arc<ArtifactEntry>],
) -> BTreeMap<ArtifactId, &CertifiedRecoveryProduct> {
    entries
        .iter()
        .filter_map(|entry| match &entry.payload {
            ArtifactPayload::Original(product) => Some((entry.descriptor.id, product)),
            _ => None,
        })
        .collect()
}

fn materialization_bytes(entries: &[Arc<ArtifactEntry>]) -> u64 {
    entries
        .iter()
        .map(|entry| match &entry.payload {
            ArtifactPayload::Original(product) => {
                product.interface_bytes().len() as u64
                    + product.product_bytes().len() as u64
                    + product.package_imports_bytes().len() as u64
                    + product.certification_bytes().len() as u64
            }
            ArtifactPayload::Canonical(interface) => {
                interface.interface_bytes().len() as u64
                    + interface.package_imports_bytes().len() as u64
                    + interface.certificate_bytes().len() as u64
                    + interface.core_bytes().map_or(0, |bytes| bytes.len() as u64)
            }
            ArtifactPayload::Interface(interface, _) => {
                interface.interface_bytes().len() as u64
                    + interface.package_imports_bytes().len() as u64
            }
        })
        .sum()
}

/// Execution recipes are selected by original artifact custody. A digest can
/// check a selected graph but cannot discover a source owner or grant visibility.
#[cfg(test)]
fn execution_scope_value(
    entries: &[Arc<ArtifactEntry>],
    root: &Path,
) -> Result<Option<Value>, CompileError> {
    execution_scope_value_with_graph_paths(entries, root, &mut BTreeMap::new(), &mut 0)
}

fn execution_scope_value_with_graph_paths(
    entries: &[Arc<ArtifactEntry>],
    root: &Path,
    graph_paths: &mut BTreeMap<[u8; 32], OwnedExecutionGraphFile>,
    written_bytes: &mut u64,
) -> Result<Option<Value>, CompileError> {
    let originals = entries
        .iter()
        .filter_map(|entry| match &entry.payload {
            ArtifactPayload::Original(product) => Some((
                (product.owner().unit.clone(), product.owner().module.clone()),
                product,
            )),
            _ => None,
        })
        .collect::<BTreeMap<_, _>>();
    let graphs = originals
        .values()
        .filter_map(|product| product.execution_source())
        .map(|graph| (graph.digest(), graph))
        .collect::<BTreeMap<_, _>>();
    if graphs.is_empty() {
        return Ok(None);
    }
    validate_execution_graph_budget(graphs.values().map(|graph| graph.as_ref()))?;
    let mut roots = Vec::new();
    let mut admitted_graphs = BTreeSet::new();
    for product in originals.values() {
        let Some(graph) = product.execution_source() else {
            continue;
        };
        let mut pending = vec![(product.owner().clone(), graph.digest())];
        let mut seen = BTreeSet::new();
        let mut closed = true;
        while let Some((owner, digest)) = pending.pop() {
            let key = (
                owner.unit.clone(),
                owner.module.clone(),
                owner.module_version.0,
                owner.skinny_iface_sha256,
                owner.product_sha256,
                digest,
            );
            if !seen.insert(key) {
                continue;
            }
            let Some(required) = originals.get(&(owner.unit.clone(), owner.module.clone())) else {
                closed = false;
                break;
            };
            let Some(required_graph) = required.execution_source() else {
                closed = false;
                break;
            };
            if required_graph
                .required_source_owners(&owner)
                .iter()
                .any(|source| {
                    originals
                        .get(&(source.unit.clone(), source.module.clone()))
                        .is_none_or(|original| original.owner() != source)
                })
                || required.owner() != &owner
                || required_graph.digest() != digest
                || !required_graph.eligible_source_replay_root(&owner)
            {
                closed = false;
                break;
            }
            pending.extend(required_graph.required_original_graphs(&owner));
        }
        if closed {
            admitted_graphs.extend(seen.into_iter().map(|entry| entry.5));
            let owner = product.owner();
            roots.push(Value::Array(vec![
                text(&owner.unit),
                text(&owner.module),
                text(hex(&owner.module_version.0)),
                text(hex(&owner.skinny_iface_sha256)),
                text(hex(&owner.product_sha256)),
                text(hex(&graph.digest())),
            ]));
        }
    }
    Ok(Some(Value::Array(vec![
        Value::Array(
            graphs
                .into_iter()
                .filter(|(digest, _)| admitted_graphs.contains(digest))
                .map(|(digest, graph)| {
                    let path = if let Some(file) = graph_paths.get(&digest) {
                        file.path.clone()
                    } else {
                        let path = root.join(format!("execution-{}.cbor", hex(&digest)));
                        let mut file = std::fs::OpenOptions::new()
                            .write(true)
                            .create_new(true)
                            .open(&path)?;
                        file.write_all(graph.bytes())?;
                        *written_bytes += graph.bytes().len() as u64;
                        graph_paths.insert(digest, OwnedExecutionGraphFile { path: path.clone() });
                        path
                    };
                    Ok(Value::Array(vec![text(hex(&digest)), path_value(&path)?]))
                })
                .collect::<Result<Vec<_>, CompileError>>()?,
        ),
        Value::Array(roots),
    ])))
}

fn encode_scope_manifest(
    mut fields: Vec<Value>,
    execution_scope: Option<Value>,
    authorization: Option<Value>,
) -> Result<Vec<u8>, CompileError> {
    fields[1] = text("9");
    fields.push(execution_scope.unwrap_or(Value::Null));
    fields.push(authorization.unwrap_or(Value::Null));
    let value = Value::Array(fields);
    let mut bytes = Vec::new();
    ciborium::ser::into_writer(&value, &mut bytes).map_err(failure)?;
    if bytes.len() > EXACT_SCOPE_BYTES_LIMIT {
        return Err(failure("scope manifest exceeds four MiB"));
    }
    Ok(bytes)
}

/// One recovery admission owns authenticated original bytes. Scoped contexts
/// share these immutable entries without reopening or decoding artifacts.
pub struct RecoveredArtifactInventory {
    producer: [u8; 32],
    entries: BTreeMap<crate::artifact_inventory::ArtifactId, Arc<ArtifactEntry>>,
    recorded_inventory: bool,
    interfaces: Vec<(
        crate::artifact_inventory::ArtifactId,
        crate::artifact_inventory::ArtifactId,
        crate::artifact_inventory::ArtifactDependency,
    )>,
}

#[derive(Debug, thiserror::Error)]
pub enum RecoveryInventoryError {
    #[error("recovery artifact verification failed")]
    Artifacts(
        Vec<(
            crate::artifact_inventory::ArtifactId,
            recovery_artifacts::RecoveryArtifactError,
        )>,
    ),
    #[error(transparent)]
    Certification(#[from] CompileError),
}

impl RecoveredArtifactInventory {
    pub fn capture(
        root: &Path,
        products: &[RecoveryArtifactRef],
        module_interfaces: &[recovery_artifacts::RecoveryModuleInterfaceRef],
        joins: &[RecoveryJoinRef],
        values: &[RecoveryValueInterfaceRef],
        descriptors: &[crate::artifact_inventory::ArtifactDescriptor],
        interfaces: &[(
            crate::artifact_inventory::ArtifactId,
            crate::artifact_inventory::ArtifactId,
            crate::artifact_inventory::ArtifactDependency,
        )],
    ) -> Result<Self, RecoveryInventoryError> {
        Self::capture_inputs(
            root,
            products,
            module_interfaces,
            joins,
            values,
            Some((descriptors, interfaces)),
        )
    }

    fn capture_inputs(
        root: &Path,
        products: &[RecoveryArtifactRef],
        module_interfaces: &[recovery_artifacts::RecoveryModuleInterfaceRef],
        joins: &[RecoveryJoinRef],
        values: &[RecoveryValueInterfaceRef],
        inventory: Option<(
            &[crate::artifact_inventory::ArtifactDescriptor],
            &[(
                crate::artifact_inventory::ArtifactId,
                crate::artifact_inventory::ArtifactId,
                crate::artifact_inventory::ArtifactDependency,
            )],
        )>,
    ) -> Result<Self, RecoveryInventoryError> {
        let mut context = ExactDeclarationContext {
            producer: [0; 32],
            original_instance_environment: OriginalInstanceEnvironment::Unknown,
            template_imports: None,
            inventory: ArtifactInventory::default().empty_view(),
            lexical: vec![],
        };
        let mut validation = PackageInterfaceValidation::default();
        let mut entries = Vec::new();
        let mut losses = Vec::new();
        let mut verified_modules = Vec::new();
        let mut selected_modules = BTreeSet::new();
        // Native references carry canonical evidence themselves. Explicit rows
        // add standalone type owners; both inputs cross the same admission.
        for reference in module_interfaces.iter().chain(
            products
                .iter()
                .filter_map(|product| product.module_interface.as_ref()),
        ) {
            if !selected_modules.insert(reference) {
                continue;
            }
            match recovery_artifacts::recover_module_interface(root, reference, &mut validation) {
                Ok(interface) => verified_modules.push((reference, interface)),
                Err(error) => losses.push((
                    crate::artifact_inventory::ArtifactDescriptor::from_recovery_module_interface(
                        reference,
                    )
                    .id,
                    error,
                )),
            }
        }
        let mut verified = Vec::new();
        for reference in products {
            let captured = reference.module_interface.as_ref().and_then(|required| {
                verified_modules
                    .iter()
                    .find(|(supplied, _)| *supplied == required)
            });
            let result = match captured {
                Some((_, interface)) => {
                    recovery_artifacts::verify_materialized_ref_with_module_interface(
                        root,
                        reference,
                        interface,
                        &mut validation,
                    )
                }
                None => Err(recovery_artifacts::RecoveryArtifactError::InvalidReference),
            };
            match result {
                Ok(artifact) => verified.push(artifact),
                Err(error) => losses.push((
                    crate::artifact_inventory::ArtifactDescriptor::from_recovery_product(reference)
                        .id,
                    error,
                )),
            }
        }
        let mut verified_joins = Vec::new();
        for reference in joins {
            match recovery_artifacts::verify_materialized_join_with_validation(
                root,
                reference,
                &mut validation,
            ) {
                Ok(artifact) => verified_joins.push(artifact),
                Err(error) => losses.push((
                    crate::artifact_inventory::ArtifactDescriptor::from_recovery_join(reference).id,
                    error,
                )),
            }
        }
        let mut verified_values = Vec::new();
        for reference in values {
            match recovery_artifacts::verify_materialized_join_with_validation(
                root,
                &reference.interface,
                &mut validation,
            ) {
                Ok(artifact) => verified_values.push(artifact),
                Err(error) => losses.push((reference.artifact_id, error)),
            }
        }
        if !losses.is_empty() {
            return Err(RecoveryInventoryError::Artifacts(losses));
        }
        let certified = crate::certified_products::certify_recovery_products_with_validation(
            verified,
            &mut validation,
        )
        .map_err(failure)?;
        for original in certified {
            context.admit_producer(original.producer_sha256)?;
            let product = original.product;
            let requirements = original.requirements;
            if product.owner() != &requirements.owner {
                return Err(failure("recovered original owner differs").into());
            }
            let entry = ArtifactEntry::original_with_validation(
                context.producer,
                product,
                &mut validation,
            )?;
            entries.push(entry);
        }
        for (reference, artifact) in joins.iter().zip(verified_joins) {
            context.admit_producer(reference.toolchain_identity_sha256)?;
            let join = CertifiedJoinedInterface::from_certification(
                reference.toolchain_identity_sha256,
                reference.unit.clone(),
                reference.module.clone(),
                artifact.interface_bytes,
                artifact.package_imports_bytes,
            )
            .map_err(failure)?;
            entries.push(ArtifactEntry::interface(
                join,
                JoinedInterfaceRole::LexicalJoin,
                Vec::new(),
            ));
        }
        for (reference, artifact) in values.iter().zip(verified_values) {
            context.admit_producer(reference.interface.toolchain_identity_sha256)?;
            let interface = CertifiedJoinedInterface::from_certification(
                reference.interface.toolchain_identity_sha256,
                reference.interface.unit.clone(),
                reference.interface.module.clone(),
                artifact.interface_bytes,
                artifact.package_imports_bytes,
            )
            .map_err(failure)?;
            let entry = ArtifactEntry::interface(
                interface,
                JoinedInterfaceRole::ValueInterface,
                reference.requirements.clone(),
            );
            if entry.descriptor.id != reference.artifact_id {
                return Err(failure("value interface artifact identity differs").into());
            }
            entries.push(entry);
        }
        for (_, interface) in verified_modules {
            context.admit_producer(interface.producer_sha256())?;
            entries.push(ArtifactEntry::canonical(interface));
        }
        // Native references carry their same canonical interface; standalone type
        // owners arrive through the explicit manifest rows above.
        let canonical = entries
            .iter()
            .filter_map(|entry| match &entry.payload {
                ArtifactPayload::Original(product) => product.module_interface().cloned(),
                _ => None,
            })
            .map(ArtifactEntry::canonical)
            .collect::<Vec<_>>();
        entries.extend(canonical);
        let mut unique = BTreeMap::new();
        for entry in entries {
            if let Some(previous) = unique.insert(entry.descriptor.id, entry.clone()) {
                if previous != entry {
                    return Err(failure("conflicting recovered canonical carrier").into());
                }
            }
        }
        let mut entries = unique.into_values().collect::<Vec<_>>();
        let mut interfaces = Vec::new();
        if let Some((descriptors, dependencies)) = inventory {
            crate::artifact_inventory::restore_recovery_interface_dependencies(
                &mut entries,
                descriptors,
                dependencies,
            )?;
            interfaces = dependencies.to_vec();
        }
        for entry in &mut entries {
            entry.requirements.sort();
            entry.requirements.dedup();
        }
        let entries = entries
            .into_iter()
            .map(|entry| (entry.descriptor.id, Arc::new(entry)))
            .collect::<BTreeMap<_, _>>();
        Ok(Self {
            producer: context.producer,
            entries,
            recorded_inventory: inventory.is_some(),
            interfaces,
        })
    }

    /// Restore persisted selection only after full products were authenticated.
    /// The raw keys grant no checked-entry or live binding authority.
    pub fn context(
        &self,
        ids: &[crate::artifact_inventory::ArtifactId],
        groups: &[crate::artifact_inventory::NativeGroupKey],
        lexical: Vec<ExactLexicalNode>,
    ) -> Result<ExactDeclarationContext, CompileError> {
        let selected = groups.iter().copied().collect::<BTreeSet<_>>();
        if selected.len() != groups.len() {
            return Err(failure("duplicate recovered native group selection"));
        }
        self.context_with_selection(ids, Some(&selected), lexical)
    }

    /// Standalone callers explicitly request every authenticated original group.
    /// Durable V7 restoration always uses `context` with its recorded selection.
    pub fn context_all_groups(
        &self,
        ids: &[crate::artifact_inventory::ArtifactId],
        lexical: Vec<ExactLexicalNode>,
    ) -> Result<ExactDeclarationContext, CompileError> {
        self.context_with_selection(ids, None, lexical)
    }

    fn context_with_selection(
        &self,
        ids: &[crate::artifact_inventory::ArtifactId],
        groups: Option<&BTreeSet<crate::artifact_inventory::NativeGroupKey>>,
        lexical: Vec<ExactLexicalNode>,
    ) -> Result<ExactDeclarationContext, CompileError> {
        let selected = ids.iter().copied().collect::<BTreeSet<_>>();
        if selected.len() != ids.len() {
            return Err(failure("duplicate recovered artifact selection"));
        }
        let entries = ids
            .iter()
            .map(|id| {
                self.entries
                    .get(id)
                    .cloned()
                    .ok_or_else(|| failure("missing recovered artifact selection"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        for (from, to, _) in &self.interfaces {
            if selected.contains(from) && !selected.contains(to) {
                return Err(failure("recovered interface closure is incomplete"));
            }
        }
        let inventory = ArtifactInventory::default();
        let empty = inventory.empty_view();
        let restored = match groups {
            Some(groups) => inventory.admit_recovery_selection(&empty, entries, groups)?,
            None => inventory.admit_shared(&empty, entries)?,
        };
        if restored
            .descriptors()
            .iter()
            .map(|row| row.id)
            .collect::<BTreeSet<_>>()
            != selected
        {
            return Err(failure(
                "recovered artifact closure differs from recorded selection",
            ));
        }
        if self.recorded_inventory {
            let expected = self
                .interfaces
                .iter()
                .filter(|(from, _, _)| selected.contains(from))
                .cloned()
                .collect::<BTreeSet<_>>();
            let actual = restored
                .interface_dependencies()
                .into_iter()
                .collect::<BTreeSet<_>>();
            if actual != expected {
                return Err(failure(
                    "recovered interface facts differ from recorded selection",
                ));
            }
        }
        let context = ExactDeclarationContext {
            producer: self.producer,
            original_instance_environment: OriginalInstanceEnvironment::Unknown,
            template_imports: None,
            inventory: restored,
            lexical,
        };
        context.normalize()?;
        Ok(context)
    }
}

/// The paths live under the caller's owned artifact directory. Their content
/// and requirements are always derived from the protected context.
pub struct MaterializedExactDeclarationContext {
    pub artifacts: Vec<DeclarationArtifact>,
    pub lexical: Vec<ExactLexicalNode>,
}

/// Private files and newly certified entries added by one immutable graph view.
/// Parents retain inherited rows and group payloads; requests assemble borrowed
/// selections without storing a copy of every ancestor in each descendant.
pub(crate) struct RetainedArtifactMaterialization {
    _directory: tempfile::TempDir,
    _parents: Vec<Arc<Self>>,
    rows: BTreeMap<ArtifactId, RetainedArtifactRow>,
    groups: Arc<[PendingCertifiedGroup]>,
    execution_scope: Option<Value>,
    graph_paths: BTreeMap<[u8; 32], OwnedExecutionGraphFile>,
    #[cfg(test)]
    payload_work: recovery_artifacts::RecoveryArtifactWork,
}

/// Issued only when the owning materialization writes an admitted immutable
/// graph. Its path is transported by the sealed exact scope, while the request
/// retains this materialization and its parent directories through worker use.
#[derive(Clone)]
struct OwnedExecutionGraphFile {
    path: PathBuf,
}

impl RetainedArtifactMaterialization {
    fn visit_owners<'a>(&'a self, seen: &mut BTreeSet<usize>, visit: &mut impl FnMut(&'a Self)) {
        let mut pending = vec![(self, false)];
        while let Some((owner, parents_visited)) = pending.pop() {
            if parents_visited {
                visit(owner);
            } else if seen.insert(owner as *const Self as usize) {
                pending.push((owner, true));
                pending.extend(
                    owner
                        ._parents
                        .iter()
                        .rev()
                        .map(|parent| (parent.as_ref(), false)),
                );
            }
        }
    }

    fn selected_rows<'a>(
        &'a self,
        metadata: &ArtifactMetadataSnapshot,
    ) -> BTreeMap<ArtifactId, &'a RetainedArtifactRow> {
        let selected = metadata
            .entries
            .values()
            .map(|entry| entry.descriptor.id)
            .collect::<BTreeSet<_>>();
        let mut rows = BTreeMap::new();
        self.visit_owners(&mut BTreeSet::new(), &mut |owner| {
            for (id, row) in &owner.rows {
                if selected.contains(id) {
                    rows.insert(*id, row);
                }
            }
        });
        rows
    }

    fn selected_group_refs<'a>(
        owners: impl IntoIterator<Item = &'a Self>,
        metadata: &ArtifactMetadataSnapshot,
    ) -> Vec<&'a PendingCertifiedGroup> {
        let available = metadata
            .artifacts
            .values()
            .filter_map(|entry| match &entry.payload {
                ArtifactPayload::Original(product) => Some((product.owner(), entry.descriptor.id)),
                _ => None,
            })
            .collect::<Vec<_>>();
        let mut groups = Vec::new();
        let mut seen = BTreeSet::new();
        let mut visited = BTreeSet::new();
        for owner in owners {
            owner.visit_owners(&mut visited, &mut |owner| {
                for group in owner.groups.iter() {
                    let native = group.owner();
                    if available
                        .iter()
                        .find_map(|(selected, artifact)| {
                            let key = NativeGroupKey {
                                artifact: *artifact,
                                original_ordinal: group.group().original_ordinal(),
                            };
                            (**selected == *native
                                && metadata.selected_native_groups.contains(&key))
                            .then_some(key)
                        })
                        .is_some_and(|key| seen.insert(key))
                    {
                        groups.push(group);
                    }
                }
            });
        }
        groups
    }

    fn graph_path_refs<'a>(&'a self) -> BTreeMap<[u8; 32], &'a OwnedExecutionGraphFile> {
        let mut paths = BTreeMap::new();
        self.visit_owners(&mut BTreeSet::new(), &mut |owner| {
            paths.extend(
                owner
                    .graph_paths
                    .iter()
                    .map(|(digest, path)| (*digest, path)),
            );
        });
        paths
    }
}

#[derive(Clone)]
struct RetainedArtifactRow {
    artifact: DeclarationArtifact,
    interface_evidence: Value,
}

/// Native generation demands belong to executable requests. A pure preview
/// observes only its mounted input and must not inherit unrelated heap inputs.
#[derive(Clone, Copy)]
pub(crate) enum RetainedGenerationPolicy {
    PreserveCertifiedDemand,
    PureActivationPreview,
}

#[derive(Clone)]
pub(crate) struct ExactCompilationRequest {
    pub(crate) context: Arc<ExactDeclarationContext>,
    pub(crate) manifest: PathBuf,
    pub(crate) request_sha256: String,
    pub(crate) semantic_sha256: [u8; 32],
    pub(crate) producer_sha256: [u8; 32],
    pub(crate) artifacts: Vec<DeclarationArtifact>,
    pub(crate) groups: Arc<[PendingCertifiedGroup]>,
    materialization: Option<Arc<RetainedArtifactMaterialization>>,
    // Only current-program source-selected support can add these roots.
    program_support: Option<ProgramSourceSupport>,
    program_source_lexical: Vec<ExactLexicalNode>,
    source_selected_support: BTreeSet<ExactModuleIdentity>,
    source_search_include: Option<Arc<[PathBuf]>>,
    checked_value_imports: crate::checked_cell::CheckedValueImportAuthority,
    generated_scaffold_imports: Vec<GeneratedScaffoldImportAuthority>,
}

/// Same-offer support keeps the consumed import graph with its exact custody.
/// Artifact dependencies alone cannot recover original instance scope.
#[derive(Clone)]
struct ProgramSourceSupport {
    artifacts: ArtifactView,
    imports: Arc<BTreeMap<ExactModuleIdentity, Vec<ExactModuleIdentity>>>,
}

impl ProgramSourceSupport {
    fn extend(
        previous: Option<&Self>,
        artifacts: ArtifactView,
        imports: impl IntoIterator<Item = (ExactModuleIdentity, Vec<ExactModuleIdentity>)>,
    ) -> Result<Self, CompileError> {
        let artifacts = match previous {
            Some(previous) => previous.artifacts.merge(&artifacts)?,
            None => artifacts,
        };
        let mut retained =
            previous.map_or_else(BTreeMap::new, |value| value.imports.as_ref().clone());
        for (owner, mut requirements) in imports {
            requirements.sort();
            requirements.dedup();
            if retained
                .insert(owner, requirements.clone())
                .is_some_and(|old| old != requirements)
            {
                return Err(failure("program support changed its original import graph"));
            }
        }
        Ok(Self {
            artifacts,
            imports: Arc::new(retained),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TemplateInterfaceNode {
    pub(crate) interface_sha256: [u8; 32],
    pub(crate) imports: Vec<ExactModuleIdentity>,
}

/// Only direct imports selected by an original checked template can become
/// roots of a later protected template. Supporting graph nodes retain types
/// and instances without acquiring an independent import capability.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct SelectedTemplateImports {
    roots: BTreeSet<ExactModuleIdentity>,
    graph: BTreeMap<ExactModuleIdentity, TemplateInterfaceNode>,
}

impl SelectedTemplateImports {
    pub(crate) fn authorization_value(&self) -> Value {
        Value::Array(vec![
            Value::Array(self.roots.iter().map(module_value).collect()),
            Value::Array(
                self.graph
                    .iter()
                    .map(|(owner, node)| {
                        Value::Array(vec![
                            text(&owner.unit),
                            text(&owner.module),
                            text(hex(&node.interface_sha256)),
                            Value::Array(node.imports.iter().map(module_value).collect()),
                        ])
                    })
                    .collect(),
            ),
        ])
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RetainedTemplateNode {
    canonical: ArtifactId,
    interface: TemplateInterfaceNode,
}

/// Live checked-value custody, issued from the original compiler receipt.
/// Neither a type projection nor recovered native availability can issue it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RetainedTemplateImports {
    producer: [u8; 32],
    roots: BTreeSet<ExactModuleIdentity>,
    graph: BTreeMap<ExactModuleIdentity, RetainedTemplateNode>,
    artifacts: ArtifactView,
}

fn template_selects_owner(templates: &[String], owner: &ExactModuleIdentity) -> bool {
    templates.iter().any(|template| {
        template
            .lines()
            .any(|line| template_import_line(line, owner))
    })
}

fn template_import_line(line: &str, owner: &ExactModuleIdentity) -> bool {
    let Some(rest) = line.trim_start().strip_prefix("import") else {
        return false;
    };
    if !rest.chars().next().is_some_and(char::is_whitespace) {
        return false;
    }
    let mut rest = rest.trim_start();
    let qualified = if let Some(qualified) = rest.strip_prefix("qualified") {
        if !qualified.chars().next().is_some_and(char::is_whitespace) {
            return false;
        }
        rest = qualified.trim_start();
        true
    } else {
        false
    };
    let Some(after_module) = rest.strip_prefix(&owner.module) else {
        return false;
    };
    if after_module.is_empty() {
        return true;
    }
    if !after_module.chars().next().is_some_and(char::is_whitespace) {
        return false;
    }
    let mut tail = after_module.trim_start();
    if qualified {
        if let Some(alias) = tail.strip_prefix("as ") {
            let alias_end = alias
                .char_indices()
                .find(|(_, character)| character.is_whitespace())
                .map_or(alias.len(), |(index, _)| index);
            let name = &alias[..alias_end];
            if name.is_empty()
                || !name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
            {
                return false;
            }
            tail = alias[alias_end..].trim_start();
        }
    }
    tail.is_empty()
        || tail.starts_with('(')
        || tail.strip_prefix("hiding").is_some_and(|items| {
            items.chars().next().is_some_and(char::is_whitespace)
                && items.trim_start().starts_with('(')
        })
}

impl RetainedTemplateImports {
    pub(crate) fn retain_in(&self, artifacts: &ArtifactView) -> Result<ArtifactView, CompileError> {
        Ok(artifacts.merge(&self.artifacts)?)
    }

    pub(crate) fn capture(
        original: &ExactDeclarationContext,
        templates: &[String],
    ) -> Result<Option<Arc<Self>>, CompileError> {
        let entries = original.artifact_view().entries();
        let canonical = entries
            .iter()
            .filter_map(|entry| match &entry.payload {
                ArtifactPayload::Canonical(interface)
                    if matches!(
                        interface.origin(),
                        crate::certified_products::CanonicalOrigin::SourceOriginal { .. }
                    ) =>
                {
                    Some((entry.descriptor.owner.clone(), entry.clone()))
                }
                _ => None,
            })
            .collect::<BTreeMap<_, _>>();
        let roots = original
            .lexical_graph()
            .iter()
            .filter(|node| {
                canonical.contains_key(&node.owner)
                    && !node.owner.module.starts_with("Tidepool.Session.")
                    && node.owner.module != "Tidepool.Internal.Resume"
                    && template_selects_owner(templates, &node.owner)
            })
            .map(|node| node.owner.clone())
            .collect::<BTreeSet<_>>();
        if roots.is_empty() {
            return Ok(None);
        }
        if !matches!(
            original.original_instance_environment,
            OriginalInstanceEnvironment::Complete { .. }
        ) {
            return Err(failure(
                "retained template imports require complete original compiler execution evidence",
            ));
        }
        let graph = original
            .interface_graph_for_roots(roots.iter().cloned().collect())?
            .into_iter()
            .map(|(owner, interface)| {
                let entry = canonical.get(&owner).ok_or_else(|| {
                    failure("original template graph lacks canonical source custody")
                })?;
                Ok((
                    owner,
                    RetainedTemplateNode {
                        canonical: entry.descriptor.id,
                        interface,
                    },
                ))
            })
            .collect::<Result<BTreeMap<_, _>, CompileError>>()?;
        let artifacts = original
            .artifact_view()
            .select_roots(graph.values().map(|node| node.canonical).collect())?;
        let retained = Arc::new(Self {
            producer: original.producer,
            roots,
            graph,
            artifacts,
        });
        retained.validate(original.producer, original.artifact_view())?;
        Ok(Some(retained))
    }

    fn validate(&self, producer: [u8; 32], artifacts: &ArtifactView) -> Result<(), CompileError> {
        if producer != self.producer
            || self.roots.is_empty()
            || self.graph.len() > 4096
            || self
                .graph
                .values()
                .map(|node| node.interface.imports.len())
                .sum::<usize>()
                > 65536
            || self.roots.iter().any(|root| !self.graph.contains_key(root))
        {
            return Err(failure("retained template roots or producer differ"));
        }
        let entries = artifacts.entries();
        let mut reachable = BTreeSet::new();
        let mut pending = self.roots.iter().cloned().collect::<Vec<_>>();
        while let Some(owner) = pending.pop() {
            if !reachable.insert(owner.clone()) {
                continue;
            }
            let node = self
                .graph
                .get(&owner)
                .ok_or_else(|| failure("retained template graph is not closed"))?;
            if !entries.iter().any(|entry| entry.descriptor.id == node.canonical
                && entry.descriptor.owner == owner
                && entry.descriptor.producer_sha256 == producer
                && entry.descriptor.interface_sha256 == node.interface.interface_sha256
                && matches!(&entry.payload, ArtifactPayload::Canonical(interface)
                    if matches!(interface.origin(), crate::certified_products::CanonicalOrigin::SourceOriginal { .. }))) {
                return Err(failure("retained template canonical identity differs"));
            }
            let imports = node.interface.imports.iter().collect::<BTreeSet<_>>();
            if imports.len() != node.interface.imports.len() {
                return Err(failure("duplicate retained template edge"));
            }
            pending.extend(node.interface.imports.iter().cloned());
        }
        if reachable.len() != self.graph.len() {
            return Err(failure("retained template graph has unselected owners"));
        }
        Ok(())
    }

    fn merge(left: &Arc<Self>, right: &Arc<Self>) -> Result<Arc<Self>, CompileError> {
        if Arc::ptr_eq(left, right) {
            return Ok(left.clone());
        }
        if left.producer != right.producer {
            return Err(failure("template producers differ"));
        }
        let mut graph = left.graph.clone();
        for (owner, node) in &right.graph {
            if graph
                .insert(owner.clone(), node.clone())
                .is_some_and(|prior| prior != *node)
            {
                return Err(failure("retained templates select conflicting originals"));
            }
        }
        let merged = Arc::new(Self {
            producer: left.producer,
            roots: left.roots.union(&right.roots).cloned().collect(),
            graph,
            artifacts: left.artifacts.merge(&right.artifacts)?,
        });
        merged.validate(merged.producer, &merged.artifacts)?;
        Ok(merged)
    }
}

/// The compiler-only import belongs to a hash-sealed checked template and one
/// original native/interface owner. It never grants authored lexical visibility.
#[derive(Clone)]
struct GeneratedScaffoldImportAuthority {
    role: GeneratedScaffoldRole,
    protected_templates: Arc<[String]>,
}

#[derive(Clone)]
enum GeneratedScaffoldRole {
    Native(NativeScaffoldRole),
    InitialTemplateInterfaces {
        producer: [u8; 32],
        roots: BTreeSet<ExactModuleIdentity>,
        graph: Arc<BTreeMap<ExactModuleIdentity, TemplateInterfaceNode>>,
    },
}

#[derive(Clone)]
enum NativeScaffoldRole {
    Resume(tidepool_repr::execution_schema::CachedHomeOwner),
    PlannedDeclaration(Arc<CertifiedAuthoredDeclaration>),
}

const GENERATED_RESUME_IMPORT: &str = "import qualified Tidepool.Internal.Resume as TidepoolResume";

impl GeneratedScaffoldImportAuthority {
    /// Match the virtual graph installed by the compiler from this protected
    /// recipe. This is implementation/instance evidence, not authored imports.
    fn original_instance_graph(&self) -> Vec<ExactLexicalNode> {
        match &self.role {
            GeneratedScaffoldRole::Native(native) => {
                let owner = match native {
                    NativeScaffoldRole::Resume(owner) => owner,
                    NativeScaffoldRole::PlannedDeclaration(certificate) => {
                        certificate.product().owner()
                    }
                };
                vec![ExactLexicalNode {
                    owner: identity(&owner.unit, &owner.module),
                    imports: Vec::new(),
                }]
            }
            GeneratedScaffoldRole::InitialTemplateInterfaces { graph, .. } => graph
                .iter()
                .map(|(owner, node)| ExactLexicalNode {
                    owner: owner.clone(),
                    imports: node.imports.clone(),
                })
                .collect(),
        }
    }

    fn permits(
        &self,
        context: &ExactDeclarationContext,
        source: &str,
        target: &Path,
        module_source: &Path,
        unit: &str,
        module: &str,
        qualifier: &str,
        boot: bool,
    ) -> bool {
        let native = match &self.role {
            GeneratedScaffoldRole::Native(native) => native,
            GeneratedScaffoldRole::InitialTemplateInterfaces {
                producer,
                roots,
                graph,
            } => {
                if target != module_source
                    || boot
                    || qualifier != "none"
                    || *producer != context.producer
                {
                    return false;
                }
                let key = identity(unit, module);
                if !roots.contains(&key) {
                    return false;
                }
                let Some(node) = graph.get(&key) else {
                    return false;
                };
                let occurrences = |text: &str| {
                    let mut counts = BTreeMap::new();
                    for line in text.lines().filter(|line| template_import_line(line, &key)) {
                        *counts.entry(line.to_owned()).or_insert(0usize) += 1;
                    }
                    counts
                };
                let rendered = occurrences(source);
                // GHC owns per-occurrence AST/span admission. This receipt
                // guard requires the whole protected owner census unchanged;
                // extra authored imports need their ordinary source proof.
                return !rendered.is_empty()
                    && self
                        .protected_templates
                        .iter()
                        .any(|template| occurrences(template) == rendered)
                    && context.artifact_view().entries().iter().any(|entry| {
                        entry.descriptor.owner == key
                            && entry.descriptor.interface_sha256 == node.interface_sha256
                    });
            }
        };
        let owner = match native {
            NativeScaffoldRole::Resume(owner) => owner,
            NativeScaffoldRole::PlannedDeclaration(certificate) => certificate.product().owner(),
        };
        if target != module_source
            || boot
            || qualifier != "none"
            || unit != owner.unit
            || module != owner.module
        {
            return false;
        }
        let compiler_import = match native {
            NativeScaffoldRole::Resume(_) => GENERATED_RESUME_IMPORT.to_owned(),
            NativeScaffoldRole::PlannedDeclaration(_) => format!("import {}", owner.module),
        };
        if source
            .lines()
            .filter(|line| *line == compiler_import)
            .count()
            != 1
        {
            return false;
        }
        if !self
            .protected_templates
            .iter()
            .any(|template| match native {
                NativeScaffoldRole::Resume(_) => {
                    template
                        .lines()
                        .filter(|line| *line == GENERATED_RESUME_IMPORT)
                        .count()
                        == 1
                }
                NativeScaffoldRole::PlannedDeclaration(_) => {
                    template
                        .lines()
                        .filter(|line| *line == "-- tidepool-preamble-imports-v1")
                        .count()
                        == 1
                }
            })
        {
            return false;
        }
        context.artifact_view().entries().iter().any(|entry| {
            matches!(&entry.payload, ArtifactPayload::Original(product) if
            match native {
                NativeScaffoldRole::Resume(owner) => product.owner() == owner,
                NativeScaffoldRole::PlannedDeclaration(certificate) =>
                    product == certificate.product()
                    && certificate.toolchain_identity_sha256() == context.producer,
            })
        })
    }
}

/// A successful compiler transaction's actual generated source, bound to its
/// immutable declaration context. It is not ordinary source-cache evidence.
#[derive(Clone, Debug)]
pub struct ExactSourceWitness {
    source_path: PathBuf,
    source_sha256: [u8; 32],
}

impl ExactSourceWitness {
    pub fn source_path(&self) -> &Path {
        &self.source_path
    }
    pub fn source_sha256(&self) -> &[u8; 32] {
        &self.source_sha256
    }
    pub fn matches_source(&self, path: &Path, source: &str) -> bool {
        use sha2::Digest;
        self.source_path == path
            && self.source_sha256 == <[u8; 32]>::from(sha2::Sha256::digest(source.as_bytes()))
    }
}

pub(crate) struct ExactSourceAdmission {
    pub(crate) witness: ExactSourceWitness,
    pub(crate) evidence: crate::cache::CompletedSourceEvidence,
    pub(crate) evidence_bytes: Vec<u8>,
    pub(crate) exact_imports: BTreeMap<ExactModuleIdentity, Vec<ExactModuleIdentity>>,
    // Roots accepted by each retained typed protected-recipe authority.
    scaffold_roots: BTreeMap<usize, BTreeSet<ExactModuleIdentity>>,
    pub(crate) exact_source_imports:
        BTreeMap<ExactModuleIdentity, Vec<crate::certified_products::CanonicalSourceImport>>,
    pub(crate) selected_originals:
        BTreeMap<ExactModuleIdentity, crate::execution_source::SourceSelectedOriginal>,
}

pub(crate) struct ExactProductAdmission<'a> {
    pub(crate) request: &'a ExactCompilationRequest,
    pub(crate) source: &'a ExactSourceAdmission,
}

/// Resolve home adjacency from the compiler's complete consumed-source graph.
/// This helper carries no admission authority; its original issuer validates
/// the evidence and interface custody before retaining the lexical surface.
pub(crate) fn consumed_source_home_imports(
    evidence: &crate::cache::DependencyEvidence,
    exact_imports: &BTreeMap<ExactModuleIdentity, Vec<ExactModuleIdentity>>,
) -> Result<BTreeMap<ExactModuleIdentity, Vec<ExactModuleIdentity>>, CompileError> {
    let source_paths = evidence
        .sources
        .iter()
        .map(|source| &source.path)
        .collect::<BTreeSet<_>>();
    let mut selected_owners = BTreeMap::new();
    for node in &evidence.modules {
        if selected_owners
            .insert((&node.unit, &node.module, node.boot, &node.source), node)
            .is_some()
        {
            return Err(failure("duplicate captured source import owner"));
        }
    }
    let mut imports = BTreeMap::new();
    for node in evidence.modules.iter().filter(|node| !node.boot) {
        if !source_paths.contains(&node.source) {
            return Err(failure(
                "original source import owner lacks its captured source",
            ));
        }
        let owner = identity(&node.unit, &node.module);
        let mut requirements = exact_imports
            .get(&owner)
            .into_iter()
            .flatten()
            .cloned()
            .collect::<BTreeSet<_>>();
        for edge in &node.imports {
            let Some(selected) = &edge.selected else {
                continue;
            };
            if !matches!(&edge.qualifier, crate::cache::ImportQualifier::Unqualified)
                && !matches!(&edge.qualifier,
                        crate::cache::ImportQualifier::ThisUnit(unit) if unit == &node.unit)
            {
                return Err(failure("selected home import has another unit qualifier"));
            }
            let Some(selected_owner) =
                selected_owners.get(&(&node.unit, &edge.module, edge.boot, selected))
            else {
                return Err(failure(
                    "selected home import lacks one captured source owner",
                ));
            };
            if !source_paths.contains(selected) {
                return Err(failure("selected home import lacks its captured source"));
            }
            requirements.insert(identity(&selected_owner.unit, &selected_owner.module));
        }
        if imports
            .insert(owner, requirements.into_iter().collect())
            .is_some()
        {
            return Err(failure("duplicate original source import owner"));
        }
    }
    Ok(imports)
}

impl ExactSourceAdmission {
    /// Completed-source validation binds GENERATED_SOURCE to this receipt's
    /// hash-verified witness. Resolve its owner without parsing source text.
    pub(crate) fn generated_source_owner(&self) -> Result<ExactModuleIdentity, CompileError> {
        let owners = self
            .evidence
            .modules
            .iter()
            .filter(|node| {
                !node.boot
                    && (node.source == Path::new(crate::cache::GENERATED_SOURCE)
                        || node.source == self.witness.source_path())
            })
            .map(|node| identity(&node.unit, &node.module))
            .collect::<Vec<_>>();
        match owners.as_slice() {
            [owner] => Ok(owner.clone()),
            _ => Err(failure(
                "generated source lacks one authenticated consumed owner",
            )),
        }
    }

    pub(crate) fn home_imports(
        &self,
    ) -> Result<BTreeMap<ExactModuleIdentity, Vec<ExactModuleIdentity>>, CompileError> {
        let mut imports = consumed_source_home_imports(&self.evidence, &self.exact_imports)?;
        for (owner, original) in &self.selected_originals {
            if imports
                .insert(owner.clone(), original.imports().to_vec())
                .is_some()
            {
                return Err(failure(
                    "fresh source collides with a source-selected original",
                ));
            }
        }
        Ok(imports)
    }

    pub(crate) fn validate_ineligible_evidence(&self, bytes: &[u8]) -> Result<(), CompileError> {
        let mut expected: crate::cache::DependencyEvidence =
            serde_json::from_slice(&self.evidence_bytes).map_err(failure)?;
        expected.cache_safe = false;
        expected.selection_complete = false;
        let actual: crate::cache::DependencyEvidence =
            serde_json::from_slice(bytes).map_err(failure)?;
        if serde_json::to_value(expected).map_err(failure)?
            != serde_json::to_value(actual).map_err(failure)?
        {
            return Err(failure(
                "source-cache-ineligible evidence differs from exact fresh proof",
            ));
        }
        Ok(())
    }
}

impl ExactProductAdmission<'_> {
    /// Retain the full consumed original interface graph before native support
    /// selection. Generated owners remain in this private execution evidence.
    pub(crate) fn original_execution_context(
        &self,
        artifacts: &ArtifactView,
    ) -> Result<Arc<ExactDeclarationContext>, CompileError> {
        let imports = self.source.home_imports()?;
        let inherited = self.request.context.as_ref();
        let exact_roots = self
            .source
            .exact_imports
            .values()
            .flatten()
            .cloned()
            .collect::<BTreeSet<_>>();
        let mut scaffold = Vec::new();
        if let Some(support) = &self.request.program_support {
            let roots = support
                .artifacts
                .root_entries()
                .iter()
                .map(|entry| entry.descriptor.owner.clone())
                .collect::<BTreeSet<_>>();
            let inherited_owners = inherited
                .lexical_graph()
                .iter()
                .map(|node| &node.owner)
                .collect::<BTreeSet<_>>();
            let mut pending = exact_roots
                .intersection(&roots)
                .cloned()
                .collect::<Vec<_>>();
            let mut seen = BTreeSet::new();
            while let Some(owner) = pending.pop() {
                if !seen.insert(owner.clone())
                    || imports.contains_key(&owner)
                    || inherited_owners.contains(&owner)
                {
                    continue;
                }
                // A missing row remains missing original evidence. Never
                // replace its unknown home imports with an empty leaf.
                if let Some(requirements) = support.imports.get(&owner) {
                    pending.extend(requirements.iter().cloned());
                    scaffold.push(ExactLexicalNode {
                        owner,
                        imports: requirements.clone(),
                    });
                }
            }
        }
        for (index, authority) in self.request.generated_scaffold_imports.iter().enumerate() {
            let graph = authority
                .original_instance_graph()
                .into_iter()
                .map(|node| (node.owner.clone(), node))
                .collect::<BTreeMap<_, _>>();
            let mut pending = self
                .source
                .scaffold_roots
                .get(&index)
                .into_iter()
                .flatten()
                .cloned()
                .collect::<Vec<_>>();
            let mut seen = BTreeSet::new();
            while let Some(owner) = pending.pop() {
                if !seen.insert(owner.clone()) || imports.contains_key(&owner) {
                    continue;
                }
                let node = graph
                    .get(&owner)
                    .ok_or_else(|| failure("protected scaffold instance graph is incomplete"))?;
                pending.extend(node.imports.iter().cloned());
                // A real consumed source row owns its actual adjacency; native
                // protected scaffold leaves apply only to source-less owners.
                scaffold.push(node.clone());
            }
        }
        self.request.checked_value_imports.validate()?;
        for (unit, module) in self.request.checked_value_imports.owners() {
            let owner = identity(unit, module);
            if !exact_roots.contains(&owner) || imports.contains_key(&owner) {
                continue;
            }
            let retained = artifacts.entries_for_owners(std::iter::once(owner.clone()))?;
            if let Some(entry) = retained.get(&owner) {
                match &entry.payload {
                    ArtifactPayload::Interface(interface, JoinedInterfaceRole::ValueInterface)
                        if self.request.checked_value_imports.matches_interface(interface) => {
                            // Compiler-issued thin value interfaces define no
                            // instances. Nominal requirements are not scope edges.
                            scaffold.push(ExactLexicalNode { owner, imports: Vec::new() });
                        }
                    _ => return Err(failure("checked value instance evidence differs from its exact interface authority")),
                }
            }
        }
        let lexical = compose_lexical_nodes(
            inherited
                .lexical_graph()
                .iter()
                .cloned()
                .chain(scaffold.iter().cloned())
                .chain(imports.iter().map(|(owner, imports)| ExactLexicalNode {
                    owner: owner.clone(),
                    imports: imports.clone(),
                }))
                .collect::<Vec<_>>()
                .iter(),
        )?;
        let mut required = imports
            .keys()
            .cloned()
            .chain(scaffold.iter().map(|node| node.owner.clone()))
            .chain(
                inherited
                    .lexical_graph()
                    .iter()
                    .map(|node| node.owner.clone()),
            )
            .collect::<BTreeSet<_>>();
        if let OriginalInstanceEnvironment::MissingOriginalOwners(owners) =
            inherited.original_instance_environment()
        {
            required.extend(owners.iter().cloned());
        }
        let context = ExactDeclarationContext::from_authenticated_execution(
            self.request.producer_sha256,
            artifacts,
            lexical,
            self.source.generated_source_owner()?,
            &required.into_iter().collect::<Vec<_>>(),
        )?;
        Ok(Arc::new(context))
    }
}

impl ExactCompilationRequest {
    /// Capture metadata from this still-live offer, never from reconstructed
    /// source or current cache state. The copy is diagnostic, not authority.
    pub(crate) fn retain_input_diagnostics(&self, destination: &Path) -> std::io::Result<()> {
        use std::io::Read;
        #[derive(serde::Serialize)]
        struct Facts<'a> {
            scope: &'static str,
            original_manifest: &'a Path,
            retained_manifest: &'static str,
            request_sha256: &'a str,
            observed_manifest_sha256: String,
            manifest_matches_request: bool,
            semantic_sha256: String,
            producer_sha256: String,
            selected_lexical_graph: &'a [ExactLexicalNode],
            retained_interfaces: &'a [DeclarationArtifact],
            source_selected_support: Vec<crate::artifact_inventory::ArtifactDescriptor>,
            checked_value_imports: Vec<(&'a str, &'a str)>,
        }
        struct BoundedBytes(Vec<u8>);
        impl Write for BoundedBytes {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                if bytes.len() > EXACT_SCOPE_BYTES_LIMIT - self.0.len() {
                    return Err(std::io::Error::other(
                        "exact request diagnostics exceed four MiB",
                    ));
                }
                self.0.extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        // Read with a limit so a changed/growing manifest cannot cause an
        // unbounded diagnostic allocation. Keep its actual bytes and hash.
        let mut bytes = Vec::new();
        std::fs::File::open(&self.manifest)?
            .take((EXACT_SCOPE_BYTES_LIMIT + 1) as u64)
            .read_to_end(&mut bytes)?;
        if bytes.len() > EXACT_SCOPE_BYTES_LIMIT {
            return Err(std::io::Error::other(
                "exact request manifest exceeds four MiB",
            ));
        }
        let directory = destination.join("exact-request");
        std::fs::create_dir(&directory)?;
        std::fs::write(directory.join("manifest.cbor"), &bytes)?;
        let support = self
            .program_support
            .as_ref()
            .map(|support| support.artifacts.root_entries())
            .unwrap_or_default();
        let checked_value_imports = self.checked_value_imports.owners().collect::<Vec<_>>();
        if self.artifacts.len() > EXACT_SCOPE_GRAPHS_LIMIT
            || self.context.lexical.len() > EXACT_SCOPE_GRAPHS_LIMIT
            || support.len() > EXACT_SCOPE_GRAPHS_LIMIT
            || checked_value_imports.len() > EXACT_SCOPE_GRAPHS_LIMIT
        {
            return Err(std::io::Error::other(
                "exact request diagnostic owners exceed 4096",
            ));
        }
        let observed_manifest_sha256 = crate::checked_cell::hash(&bytes);
        let facts = Facts {
            scope: "original compiler offer; diagnostic only; never compiler authority",
            original_manifest: &self.manifest,
            retained_manifest: "manifest.cbor",
            manifest_matches_request: observed_manifest_sha256 == self.request_sha256,
            observed_manifest_sha256,
            request_sha256: &self.request_sha256,
            semantic_sha256: hex(&self.semantic_sha256),
            producer_sha256: hex(&self.producer_sha256),
            selected_lexical_graph: &self.context.lexical,
            retained_interfaces: &self.artifacts,
            source_selected_support: support
                .iter()
                .map(|entry| entry.descriptor.clone())
                .collect(),
            checked_value_imports,
        };
        let mut encoded = BoundedBytes(Vec::new());
        serde_json::to_writer_pretty(&mut encoded, &facts).map_err(std::io::Error::other)?;
        std::fs::write(directory.join("facts.json"), encoded.0)
    }

    pub(crate) fn with_source_search_context(mut self, include: &[PathBuf]) -> Self {
        self.source_search_include = Some(Arc::from(include));
        self
    }

    pub(crate) fn apply_to(
        &self,
        command: &mut tidepool_extract_cmd::ExtractCmd,
        retained_policy: RetainedGenerationPolicy,
    ) -> Result<(), CompileError> {
        let request = tidepool_extract_cmd::ExtractRequest::decode(&command.request_bytes())
            .map_err(failure)?;
        let retained = retained_generation_inputs(
            retained_policy,
            request.retained_generations(),
            self.groups.iter().flat_map(PendingCertifiedGroup::imports),
        )?;
        // These authenticated tags keep original native demands intact through
        // projection. They grant neither lexical imports nor live heap roots.
        for (identity, generation) in retained {
            command.retained_generation(identity, generation);
        }
        command.session_artifacts(&self.manifest);
        Ok(())
    }

    pub(crate) fn with_checked_value_imports(
        mut self,
        authority: crate::checked_cell::CheckedValueImportAuthority,
    ) -> Self {
        self.checked_value_imports = authority;
        self
    }
    pub(crate) fn with_generated_scaffold_imports<'a>(
        mut self,
        templates: impl IntoIterator<Item = &'a str>,
    ) -> Self {
        let protected_templates = templates
            .into_iter()
            .filter(|template| {
                template
                    .lines()
                    .filter(|line| *line == GENERATED_RESUME_IMPORT)
                    .count()
                    == 1
            })
            .map(str::to_owned)
            .collect::<Vec<_>>();
        if !protected_templates.is_empty() {
            let originals = self.context.artifact_view().entries();
            let owners = originals
                .iter()
                .filter_map(|entry| match &entry.payload {
                    ArtifactPayload::Original(product)
                        if product.owner().unit == "main"
                            && product.owner().module == "Tidepool.Internal.Resume" =>
                    {
                        Some(product.owner())
                    }
                    _ => None,
                })
                .collect::<Vec<_>>();
            if let [owner] = owners.as_slice() {
                self.generated_scaffold_imports
                    .push(GeneratedScaffoldImportAuthority {
                        role: GeneratedScaffoldRole::Native(NativeScaffoldRole::Resume(
                            (*owner).clone(),
                        )),
                        protected_templates: protected_templates.into(),
                    });
            }
        }
        self
    }
    pub(crate) fn with_initial_template_interfaces(
        mut self,
        initial: Arc<ExactDeclarationContext>,
        templates: &[String],
    ) -> Result<Self, CompileError> {
        let selected = initial.selected_template_imports(templates)?;
        if selected.roots.is_empty() {
            return Ok(self);
        }
        if initial.producer != self.producer_sha256 {
            return Err(failure("checked template interface producer differs"));
        }
        let entries = self.context.artifact_view().entries();
        for (owner, node) in &selected.graph {
            if !entries.iter().any(|entry| {
                entry.descriptor.owner == *owner
                    && entry.descriptor.interface_sha256 == node.interface_sha256
            }) || self
                .context
                .lexical_graph()
                .iter()
                .any(|current| current.owner == *owner && current.imports != node.imports)
            {
                return Err(failure(
                    "checked template interface differs from its initial selection",
                ));
            }
            if initial
                .template_imports
                .as_ref()
                .and_then(|retained| retained.graph.get(owner))
                .is_some_and(|retained| {
                    !entries
                        .iter()
                        .any(|entry| entry.descriptor.id == retained.canonical)
                })
            {
                return Err(failure(
                    "checked template canonical original differs from its retained selection",
                ));
            }
        }
        self.generated_scaffold_imports
            .push(GeneratedScaffoldImportAuthority {
                role: GeneratedScaffoldRole::InitialTemplateInterfaces {
                    producer: initial.producer,
                    roots: selected.roots,
                    graph: Arc::new(selected.graph),
                },
                protected_templates: templates.to_vec().into(),
            });
        Ok(self)
    }

    pub(crate) fn with_generated_planned_imports<'a>(
        mut self,
        certificate: Option<&Arc<CertifiedAuthoredDeclaration>>,
        templates: impl IntoIterator<Item = &'a str>,
    ) -> Result<Self, CompileError> {
        if let Some(certificate) = certificate {
            if certificate.toolchain_identity_sha256() != self.producer_sha256
                || !self
                    .context
                    .recovery_products()
                    .iter()
                    .any(|product| product == certificate.product())
            {
                return Err(failure(
                    "checked recipe original differs from its admitted certificate",
                ));
            }
            let protected_templates = templates.into_iter().map(str::to_owned).collect::<Vec<_>>();
            self.generated_scaffold_imports
                .push(GeneratedScaffoldImportAuthority {
                    role: GeneratedScaffoldRole::Native(NativeScaffoldRole::PlannedDeclaration(
                        certificate.clone(),
                    )),
                    protected_templates: protected_templates.into(),
                });
        }
        Ok(self)
    }

    pub(crate) fn in_program_context(
        &self,
        root: &Path,
        context: Arc<ExactDeclarationContext>,
    ) -> Result<Self, CompileError> {
        if context.toolchain_identity_sha256() != self.producer_sha256
            && !(context.toolchain_identity_sha256() == [0; 32]
                && context.artifact_view().is_empty())
        {
            return Err(failure("program context has another producer"));
        }
        // Inventory custody grows by immutable artifact ID, but materialized
        // inputs follow its selected owner projection. An original product can
        // become available for an already retained canonical interface.
        let baseline = self.context.inventory.metadata_snapshot();
        let current = context.inventory.metadata_snapshot();
        baseline.validate_native_selection()?;
        current.validate_native_selection()?;
        let baseline_ids = baseline.artifacts.keys().copied().collect::<BTreeSet<_>>();
        let current_ids = current.artifacts.keys().copied().collect::<BTreeSet<_>>();
        if !baseline_ids.is_subset(&current_ids) {
            return Err(failure("program context removed an admitted artifact"));
        }
        if !baseline
            .selected_native_groups
            .is_subset(&current.selected_native_groups)
        {
            return Err(failure("program context removed an admitted native group"));
        }
        let new_entries = current
            .entries
            .values()
            .filter(|entry| !baseline_ids.contains(&entry.descriptor.id))
            .cloned()
            .collect::<Vec<_>>();
        let replaced_owners = new_entries
            .iter()
            .filter(|entry| baseline.entries.contains_key(&entry.descriptor.owner))
            .map(|entry| entry.descriptor.owner.clone())
            .collect::<BTreeSet<_>>();
        if !replaced_owners.is_empty() {
            // Displaced paths were consumed by earlier segments. Validate them
            // before replacing their rows so promotion cannot hide tampering.
            self.context
                .validate_artifacts_from_metadata(&self.artifacts, &baseline)?;
        }
        let delta_bytes = materialization_bytes(&new_entries);
        if !new_entries.is_empty() {
            std::fs::create_dir_all(root)?;
        }
        let delta_start = std::time::Instant::now();
        let mut validation = PackageInterfaceValidation::default();
        let (mut materialized, _) = context.materialize_entries_with_validation(
            root,
            &new_entries,
            &mut validation,
            MaterializationMode::Scratch,
        )?;
        crate::timing::record_stage(
            crate::timing::NO_NODE,
            crate::timing::NO_ROUND,
            "exact.program_delta_materialize",
            delta_start.elapsed(),
            delta_bytes,
        );
        materialized.artifacts.extend(
            self.artifacts
                .iter()
                .filter(|artifact| {
                    !replaced_owners.contains(&identity(
                        &artifact.interface.unit,
                        &artifact.interface.module,
                    ))
                })
                .cloned(),
        );
        let certify_start = std::time::Instant::now();
        let current_entries = current.artifacts.values().cloned().collect::<Vec<_>>();
        let available = original_products_by_id(&current_entries);
        let additional = certify_selected_owned_products_in_context_with_validation(
            &available,
            &self.groups,
            &current.selected_native_groups,
            &mut validation,
        )
        .map_err(failure)?;
        crate::timing::record_stage(
            crate::timing::NO_NODE,
            crate::timing::NO_ROUND,
            "exact.program_delta_certify",
            certify_start.elapsed(),
            delta_bytes,
        );
        let groups = if additional.is_empty() {
            Arc::clone(&self.groups)
        } else {
            let mut groups = self.groups.to_vec();
            groups.extend(additional);
            groups.into()
        };
        crate::timing::record_stage(
            crate::timing::NO_NODE,
            crate::timing::NO_ROUND,
            "exact.program_delta",
            delta_start.elapsed(),
            delta_bytes,
        );
        Ok(Self {
            context,
            manifest: self.manifest.clone(),
            request_sha256: self.request_sha256.clone(),
            semantic_sha256: self.semantic_sha256,
            producer_sha256: self.producer_sha256,
            artifacts: materialized.artifacts,
            groups: groups.into(),
            materialization: self.materialization.clone(),
            program_support: self.program_support.clone(),
            program_source_lexical: self.program_source_lexical.clone(),
            source_selected_support: self.source_selected_support.clone(),
            source_search_include: self.source_search_include.clone(),
            checked_value_imports: self.checked_value_imports.clone(),
            generated_scaffold_imports: self.generated_scaffold_imports.clone(),
        })
    }

    pub(crate) fn admit_program_support(
        &mut self,
        context: Arc<ExactDeclarationContext>,
        support: &ArtifactView,
        admissions: &[ExactSourceAdmission],
        produced_types: Option<&crate::checked_cell::ProducedValueTypeInterfaces>,
    ) -> Result<Arc<ExactDeclarationContext>, CompileError> {
        let mut imports = BTreeMap::new();
        let mut selected_originals = BTreeMap::new();
        for admission in admissions {
            for (owner, original) in &admission.selected_originals {
                if selected_originals
                    .insert(owner.clone(), original.clone())
                    .is_some_and(
                        |previous: crate::execution_source::SourceSelectedOriginal| {
                            previous.interface() != original.interface()
                        },
                    )
                {
                    return Err(failure(
                        "program support changed a source-selected original",
                    ));
                }
            }
            for (owner, requirements) in admission.home_imports()? {
                if imports
                    .insert(owner, requirements.clone())
                    .is_some_and(|old| old != requirements)
                {
                    return Err(failure(
                        "program support original changed selected home imports",
                    ));
                }
            }
        }
        let supplied = support
            .entries_for_owners(support.descriptors().into_iter().map(|entry| entry.owner))?;
        let retained = context
            .artifact_view()
            .entries_for_owners(supplied.keys().cloned())?;
        let fresh = supplied
            .keys()
            .filter(|owner| {
                imports.contains_key(*owner) && !selected_originals.contains_key(*owner)
            })
            .cloned()
            .collect::<Vec<_>>();
        for (owner, entry) in &supplied {
            // Reserved same-request output certificates retain type closure.
            // They do not supply source admissions or become lexical roots.
            if produced_types.is_some_and(|outputs| outputs.matches_artifact(entry)) {
                if imports.contains_key(owner) || selected_originals.contains_key(owner) {
                    return Err(failure("produced type output became source support"));
                }
                continue;
            }
            if imports.contains_key(owner) {
                if let Some(selected) = selected_originals.get(owner) {
                    let Some(previous) = retained.get(owner) else {
                        return Err(failure(
                            "source-selected support is not an existing original",
                        ));
                    };
                    if canonical_source_interface(previous) != Some(selected.interface())
                        || canonical_source_interface(entry) != Some(selected.interface())
                    {
                        return Err(failure(
                            "source-selected support has another original owner",
                        ));
                    }
                } else if retained.contains_key(owner) {
                    return Err(failure(
                        "program support cannot select a retained hidden owner",
                    ));
                } else if canonical_source_interface(entry).is_none() {
                    return Err(failure(
                        "fresh program support lacks a canonical source seal",
                    ));
                }
            } else if !retained.contains_key(owner) {
                return Err(failure("program support lacks its fresh source admission"));
            } else if canonical_source_interface(entry).is_none()
                && retained[owner].descriptor.id != entry.descriptor.id
            {
                return Err(failure(
                    "program support changed an inherited synthetic interface",
                ));
            }
        }
        for (owner, entry) in &supplied {
            if matches!(entry.payload, ArtifactPayload::Original(_)) {
                if let Some(previous) = retained.get(owner) {
                    match &previous.payload {
                        ArtifactPayload::Original(_)
                            if previous.descriptor.id != entry.descriptor.id =>
                        {
                            return Err(failure("supporting original differs from retained owner"));
                        }
                        ArtifactPayload::Canonical(interface)
                            if canonical_source_interface(entry) != Some(interface) =>
                        {
                            return Err(failure(
                                "supporting original differs from canonical module",
                            ));
                        }
                        ArtifactPayload::Interface(_, _) => {
                            return Err(failure(
                                "supporting original collides with synthetic interface",
                            ));
                        }
                        _ => {}
                    }
                }
            }
        }
        let extend_start = std::time::Instant::now();
        let mut context = (*context).clone();
        context.admit_producer(self.producer_sha256)?;
        for descriptor in support.descriptors() {
            context.admit_producer(descriptor.producer_sha256)?;
        }
        context.inventory = context.inventory.merge(support)?;
        let context = Arc::new(context);
        crate::timing::record_stage(
            crate::timing::NO_NODE,
            crate::timing::NO_ROUND,
            "exact.program_support_extend",
            extend_start.elapsed(),
            0,
        );
        if fresh.is_empty() && selected_originals.is_empty() {
            return Ok(context);
        }
        let mut inherited = context
            .lexical_graph()
            .iter()
            .filter(|node| !node.owner.module.starts_with("Tidepool.Session."))
            .map(|node| (node.owner.clone(), node.imports.clone()))
            .collect::<BTreeMap<_, _>>();
        for node in &self.program_source_lexical {
            if inherited
                .insert(node.owner.clone(), node.imports.clone())
                .is_some_and(|previous| previous != node.imports)
            {
                return Err(failure("program support changed its selected source graph"));
            }
        }
        let roots = fresh
            .iter()
            .cloned()
            // This helper is selected by executable scaffolding. Its native
            // custody does not introduce an authored import into later cells.
            .filter(|owner| owner.unit != "main" || owner.module != "Tidepool.Internal.Resume")
            // Selected originals may be source roots without any fresh product;
            // their adjacency comes from the validated current-source receipt.
            .chain(selected_originals.keys().cloned())
            .collect::<Vec<_>>();
        let implementations = context.artifact_view().source_implementation_roles();
        let program_source_lexical = crate::declaration_join::source_lexical_surface(
            &roots,
            &imports,
            &inherited
                .into_iter()
                .map(|(owner, imports)| ExactLexicalNode { owner, imports })
                .collect::<Vec<_>>(),
            &implementations,
        )?
        .lexical;
        let entries = context.artifact_view().entries_for_owners(
            fresh
                .iter()
                .cloned()
                .chain(selected_originals.keys().cloned()),
        )?;
        for (owner, selected) in &selected_originals {
            let Some(entry) = entries.get(owner) else {
                return Err(failure(
                    "source-selected support lacks its original inventory entry",
                ));
            };
            if canonical_source_interface(entry) != Some(selected.interface()) {
                return Err(failure(
                    "source-selected support cannot replace an original",
                ));
            }
        }
        let fresh = context
            .artifact_view()
            .select_roots(entries.values().map(|entry| entry.descriptor.id).collect())?;
        let fresh_owners = fresh
            .descriptors()
            .into_iter()
            .map(|entry| entry.owner)
            .collect::<BTreeSet<_>>();
        let program_support = ProgramSourceSupport::extend(
            self.program_support.as_ref(),
            fresh,
            imports
                .into_iter()
                .filter(|(owner, _)| fresh_owners.contains(owner)),
        )?;
        self.program_source_lexical = program_source_lexical;
        self.program_support = Some(program_support);
        self.source_selected_support
            .extend(selected_originals.into_keys());
        Ok(context)
    }

    pub(crate) fn program_source_lexical(&self) -> &[ExactLexicalNode] {
        &self.program_source_lexical
    }

    pub(crate) fn admit_source(
        &self,
        source_path: &Path,
        source: &str,
        fresh_evidence: &[u8],
    ) -> Result<ExactSourceAdmission, CompileError> {
        self.admit_source_with_validation(
            source_path,
            source,
            fresh_evidence,
            &mut PackageInterfaceValidation::default(),
        )
    }

    pub(crate) fn admit_source_with_validation(
        &self,
        source_path: &Path,
        source: &str,
        fresh_evidence: &[u8],
        validation: &mut PackageInterfaceValidation,
    ) -> Result<ExactSourceAdmission, CompileError> {
        use sha2::Digest;
        let expected: [u8; 32] = sha2::Sha256::digest(source.as_bytes()).into();
        self.validate_outputs_selected_with_validation(
            source_path
                .parent()
                .ok_or_else(|| failure("source has no directory"))?,
            None,
            &self.context,
            validation,
        )?
        .into_iter()
        .find(|admitted| {
            admitted.witness.source_path() == source_path
                && admitted.witness.source_sha256() == &expected
                && admitted
                    .validate_ineligible_evidence(fresh_evidence)
                    .is_ok()
        })
        .ok_or_else(|| {
            failure("source and final product evidence lack their exact consumed receipt")
        })
    }
    pub(crate) fn validate_outputs(
        &self,
        root: &Path,
    ) -> Result<Vec<ExactSourceAdmission>, CompileError> {
        self.validate_outputs_with_planned(root, None)
    }

    pub(crate) fn validate_outputs_with_planned(
        &self,
        root: &Path,
        planned: Option<&CertifiedAuthoredDeclaration>,
    ) -> Result<Vec<ExactSourceAdmission>, CompileError> {
        let Some(planned) = planned else {
            return self.validate_outputs_selected(root, None, &self.context);
        };
        if planned.toolchain_identity_sha256() != self.producer_sha256 {
            return Err(failure("planned source support has another producer"));
        }
        // The worker checks the remaining cell against these same-request
        // fresh originals. Retained hidden dependencies are not selected roots.
        let view = planned.artifact_view();
        let retained = self.context.artifact_view().entries_for_owners(
            planned
                .original_home_imports()
                .map(|(owner, _)| owner.clone()),
        )?;
        let entries = view.entries_for_owners(
            planned
                .original_home_imports()
                .filter(|(owner, _)| !retained.contains_key(*owner))
                .map(|(owner, _)| owner.clone()),
        )?;
        let support =
            view.select_roots(entries.values().map(|entry| entry.descriptor.id).collect())?;
        let mut request = self.clone();
        request.program_support = Some(ProgramSourceSupport::extend(
            self.program_support.as_ref(),
            support,
            planned
                .original_home_imports()
                .map(|(owner, imports)| (owner.clone(), imports.to_vec()))
                .chain(
                    planned
                        .source_lexical_imports()
                        .iter()
                        .map(|node| (node.owner.clone(), node.imports.clone())),
                ),
        )?);
        let owner = identity(
            &planned.product().owner().unit,
            &planned.product().owner().module,
        );
        request.validate_outputs_selected(root, Some(&owner), &self.context)
    }

    pub(crate) fn validate_outputs_in_context(
        &self,
        root: &Path,
        context: &ExactDeclarationContext,
    ) -> Result<Vec<ExactSourceAdmission>, CompileError> {
        if context.toolchain_identity_sha256() != self.producer_sha256
            && !(context.toolchain_identity_sha256() == [0; 32]
                && context.artifact_view().is_empty())
        {
            return Err(failure("same-transaction context has another producer"));
        }
        self.validate_outputs_selected(root, None, context)
    }

    fn validate_outputs_selected(
        &self,
        root: &Path,
        planned: Option<&ExactModuleIdentity>,
        context: &ExactDeclarationContext,
    ) -> Result<Vec<ExactSourceAdmission>, CompileError> {
        self.validate_outputs_selected_with_validation(
            root,
            planned,
            context,
            &mut PackageInterfaceValidation::default(),
        )
    }
    fn validate_outputs_selected_with_validation(
        &self,
        root: &Path,
        planned: Option<&ExactModuleIdentity>,
        context: &ExactDeclarationContext,
        validation: &mut PackageInterfaceValidation,
    ) -> Result<Vec<ExactSourceAdmission>, CompileError> {
        let context_validate_start = std::time::Instant::now();
        self.context.validate_artifacts(&self.artifacts)?;
        self.checked_value_imports.validate()?;
        if sha256(
            &crate::certified_products::read_bounded_with_operation(
                &self.manifest,
                EXACT_SCOPE_BYTES_LIMIT as u64,
                &validation.inventory,
            )
            .map_err(compiler_evidence_failure)?,
        ) != self.request_sha256
        {
            return Err(failure("scope request changed during compilation"));
        }
        crate::timing::record_stage(
            crate::timing::NO_NODE,
            crate::timing::NO_ROUND,
            "exact.context_validate",
            context_validate_start.elapsed(),
            0,
        );
        let receipt_validate_start = std::time::Instant::now();
        let directory = root.join(".exact-compilations");
        let mut receipts = std::fs::read_dir(&directory)
            .map_err(|error| {
                failure(format!(
                    "exact compile receipts {}: {error}",
                    directory.display()
                ))
            })?
            .map(|entry| {
                validation
                    .inventory
                    .reserve::<PathBuf>(1)
                    .map_err(|error| compiler_evidence_failure(error.into()))?;
                let path = entry?.path();
                validation
                    .inventory
                    .charge(path.as_os_str().len())
                    .map_err(|error| compiler_evidence_failure(error.into()))?;
                Ok::<_, CompileError>(path)
            })
            .collect::<Result<Vec<_>, _>>()?;
        receipts.sort();
        if receipts.is_empty() || receipts.len() > 4096 {
            return Err(failure("missing or excessive successful compile receipts"));
        }
        let admitted = receipts
            .iter()
            .map(|path| {
                self.validate_receipt_with_validation(
                    &path.join("receipt.cbor"),
                    planned,
                    context,
                    validation,
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        crate::timing::record_stage(
            crate::timing::NO_NODE,
            crate::timing::NO_ROUND,
            "exact.receipt_validate",
            receipt_validate_start.elapsed(),
            0,
        );
        Ok(admitted)
    }

    #[cfg(test)]
    fn validate_receipt(
        &self,
        path: &Path,
        planned: Option<&ExactModuleIdentity>,
        context: &ExactDeclarationContext,
    ) -> Result<ExactSourceAdmission, CompileError> {
        self.validate_receipt_with_validation(
            path,
            planned,
            context,
            &mut PackageInterfaceValidation::default(),
        )
    }

    fn validate_receipt_with_validation(
        &self,
        path: &Path,
        planned: Option<&ExactModuleIdentity>,
        context: &ExactDeclarationContext,
        validation: &mut PackageInterfaceValidation,
    ) -> Result<ExactSourceAdmission, CompileError> {
        let DecodedExactCompilationReceipt {
            source_path,
            source_sha256,
            source,
            evidence_bytes,
            evidence,
            edges,
            claims,
            selection_evidence,
        } = decode_exact_compilation_receipt_with_operation(
            path,
            Some((&self.request_sha256, self.semantic_sha256)),
            &validation.inventory,
        )?;
        let evidence = crate::cache::CompletedSourceEvidence::from_worker_evidence(
            evidence,
            &source_path,
            &source,
        )
        .ok_or_else(|| failure("fresh compilation consumed source evidence is invalid"))?;
        let interfaces = context.interface_owners();
        let mut exact_owners: BTreeSet<_> = interfaces
            .iter()
            .map(|interface| {
                (
                    interface.owner.unit.as_str(),
                    interface.owner.module.as_str(),
                )
            })
            .collect();
        if let Some(planned) = planned {
            exact_owners.insert((planned.unit.as_str(), planned.module.as_str()));
        }
        exact_owners.extend(self.checked_value_imports.owners());
        let source_owners: BTreeSet<_> = evidence
            .modules
            .iter()
            .map(|module| (module.unit.as_str(), module.module.as_str(), module.boot))
            .collect();
        if source_owners
            .iter()
            .any(|(unit, module, _)| exact_owners.contains(&(*unit, *module)))
        {
            return Err(failure("fresh module replaced an admitted exact owner"));
        }
        let mut selected: BTreeSet<_> = context
            .lexical
            .iter()
            .map(|node| (node.owner.unit.as_str(), node.owner.module.as_str()))
            .collect();
        let support_entries = self
            .program_support
            .as_ref()
            .map(|support| support.artifacts.root_entries())
            .unwrap_or_default();
        selected.extend(
            support_entries
                .iter()
                .filter(|entry| {
                    !self
                        .source_selected_support
                        .contains(&entry.descriptor.owner)
                })
                .map(|entry| {
                    (
                        entry.descriptor.owner.unit.as_str(),
                        entry.descriptor.owner.module.as_str(),
                    )
                }),
        );
        selected.extend(self.checked_value_imports.owners());
        if let Some(planned) = planned {
            selected.insert((planned.unit.as_str(), planned.module.as_str()));
        }
        let source_selection_roots = edges
            .iter()
            .flat_map(|module| {
                module.imports.iter().filter(|edge| !edge.boot).map(|edge| {
                    (
                        module.owner.clone(),
                        edge.qualifier.clone(),
                        identity(&edge.unit, &edge.module),
                    )
                })
            })
            .collect::<Vec<_>>();
        let selected_originals = if claims.is_empty() {
            BTreeMap::new()
        } else {
            let include = self.source_search_include.as_deref().ok_or_else(|| {
                failure("source-selected originals lack trusted current import roots")
            })?;
            let selection_evidence = selection_evidence
                .as_ref()
                .ok_or_else(|| failure("selected originals lack source evidence"))?;
            // Earlier admitted program support carries the same original proof
            // as persisted context. This receipt cannot authorize itself.
            let source_view = match &self.program_support {
                Some(support) => context.artifact_view().merge(&support.artifacts)?,
                None => context.artifact_view().clone(),
            };
            let entries = source_view.entries();
            let canonical = entries
                .iter()
                .filter_map(|entry| canonical_source_interface(entry))
                .map(|interface| {
                    (
                        identity(interface.unit(), interface.module()),
                        interface.clone(),
                    )
                })
                .collect::<BTreeMap<_, _>>()
                .into_values()
                .collect::<Vec<_>>();
            let exact_interfaces = entries
                .iter()
                .map(|entry| {
                    (
                        entry.descriptor.owner.clone(),
                        entry.descriptor.interface_sha256,
                    )
                })
                .collect();
            let independent = selected
                .iter()
                .map(|(unit, module)| identity(unit, module))
                .collect();
            crate::execution_source::validate_source_selected_originals_with_validation(
                claims,
                selection_evidence,
                crate::execution_source::SourceSelectionContext {
                    producer: self.producer_sha256,
                    interfaces: &canonical,
                    exact_interfaces: &exact_interfaces,
                    include,
                    fresh: &evidence,
                    roots: &source_selection_roots,
                    independent: &independent,
                },
                validation,
            )?
        };
        let mut seen = BTreeSet::new();
        let mut exact_imports = BTreeMap::new();
        let mut exact_source_imports = BTreeMap::new();
        let mut scaffold_roots: BTreeMap<usize, BTreeSet<ExactModuleIdentity>> = BTreeMap::new();
        for module in &edges {
            let owner = (
                module.owner.unit.as_str(),
                module.owner.module.as_str(),
                module.boot,
            );
            if !source_owners.contains(&owner) || !seen.insert(owner) {
                return Err(failure(
                    "exact import witness has another fresh source owner",
                ));
            }
            let mut imported = BTreeSet::new();
            let mut resolved = BTreeSet::new();
            let mut source_imports = Vec::new();
            for edge in &module.imports {
                let qualifier = String::from(edge.qualifier.clone());
                let name = edge.module.as_str();
                let boot = edge.boot;
                let unit = edge.unit.as_str();
                let mut scaffold_import = false;
                for (index, authority) in self.generated_scaffold_imports.iter().enumerate() {
                    let permitted = evidence
                        .modules
                        .iter()
                        .find(|module| {
                            module.unit == owner.0
                                && module.module == owner.1
                                && module.boot == owner.2
                        })
                        .is_some_and(|module| {
                            // Completed-source validation bound this marker
                            // to the hash-verified receipt target snapshot.
                            let module_source =
                                if module.source == Path::new(crate::cache::GENERATED_SOURCE) {
                                    &source_path
                                } else {
                                    &module.source
                                };
                            authority.permits(
                                context,
                                &source,
                                &source_path,
                                module_source,
                                unit,
                                name,
                                &qualifier,
                                boot,
                            )
                        });
                    if permitted {
                        scaffold_roots
                            .entry(index)
                            .or_default()
                            .insert(identity(unit, name));
                        scaffold_import = true;
                    }
                }
                if boot
                    || !(selected.contains(&(unit, name))
                        || scaffold_import
                        || selected_originals.contains_key(&identity(unit, name)))
                    || (qualifier != "none" && qualifier != format!("this:{unit}"))
                    || !imported.insert((qualifier.clone(), name.to_owned(), boot, unit.to_owned()))
                {
                    return Err(failure(format!(
                        "exact import witness leaves selected lexical graph: source {}:{}, import {unit}:{name}, qualifier {qualifier}, boot {boot}, selected {}",
                        owner.0,
                        owner.1,
                        selected.contains(&(unit, name)),
                    )));
                }
                resolved.insert(identity(unit, name));
                source_imports.push(crate::certified_products::CanonicalSourceImport {
                    qualifier: edge.qualifier.clone(),
                    module: name.to_owned(),
                    boot,
                    home_unit: Some(unit.to_owned()),
                });
            }
            exact_source_imports.insert(identity(owner.0, owner.1), source_imports);
            exact_imports.insert(identity(owner.0, owner.1), resolved.into_iter().collect());
        }
        if seen != source_owners {
            return Err(failure("exact import witness omits a fresh source module"));
        }
        Ok(ExactSourceAdmission {
            witness: ExactSourceWitness {
                source_path,
                source_sha256,
            },
            evidence,
            evidence_bytes,
            exact_imports,
            scaffold_roots,
            exact_source_imports,
            selected_originals,
        })
    }
}

/// Decoded wire facts. Admission separately authenticates the request and selected owners.
pub(crate) struct DecodedExactCompilationReceipt {
    pub(crate) source_path: PathBuf,
    source_sha256: [u8; 32],
    source: String,
    evidence_bytes: Vec<u8>,
    pub(crate) evidence: crate::cache::DependencyEvidence,
    edges: Vec<ExactReceiptModule>,
    pub(crate) claims: Vec<crate::execution_source::SourceSelectedOriginalClaim>,
    selection_evidence: Option<crate::cache::DependencyEvidence>,
}
struct ExactReceiptModule {
    owner: ExactModuleIdentity,
    boot: bool,
    imports: Vec<ExactReceiptImport>,
}
struct ExactReceiptImport {
    qualifier: crate::cache::ImportQualifier,
    module: String,
    boot: bool,
    unit: String,
}

#[cfg(test)]
pub(crate) fn read_exact_compilation_receipt(
    path: &Path,
) -> Result<DecodedExactCompilationReceipt, CompileError> {
    decode_exact_compilation_receipt(path, None)
}

fn decode_exact_compilation_receipt(
    path: &Path,
    expected: Option<(&str, [u8; 32])>,
) -> Result<DecodedExactCompilationReceipt, CompileError> {
    decode_exact_compilation_receipt_with_operation(
        path,
        expected,
        &tidepool_repr::execution_schema::InventoryOperation::new(Default::default()),
    )
}
fn decode_exact_compilation_receipt_with_operation(
    path: &Path,
    expected: Option<(&str, [u8; 32])>,
    operation: &tidepool_repr::execution_schema::InventoryOperation,
) -> Result<DecodedExactCompilationReceipt, CompileError> {
    use sha2::Digest;
    // A receipt retains complete fresh and source-selected dependency
    // inventories, rather than the exact scope's bounded owner descriptors.
    let limit =
        crate::certified_products::COMPILER_RECEIPT_BYTES_LIMIT.min(operation.limits().max_bytes);
    let bytes =
        crate::certified_products::read_bounded_with_operation(path, limit as u64, operation)
            .map_err(compiler_evidence_failure)?;
    let value = operation
        .decode_value(&bytes, limit)
        .map_err(|error| compiler_evidence_failure(error.into()))?;
    operation
        .charge_value_copies(&value, 3)
        .map_err(|error| compiler_evidence_failure(error.into()))?;
    let header = row(&value, 10)?;
    if string(&header[0])? != "TPEXACTCOMPILE" || string(&header[1])? != "3" {
        return Err(failure(
            "compile receipt belongs to another context or version",
        ));
    }
    let request_sha256 = string(&header[2])?.to_owned();
    crate::execution_source::parse_digest(&request_sha256)?;
    let semantic_sha256 = crate::execution_source::parse_digest(string(&header[3])?)?;
    if expected
        .is_some_and(|(request, semantic)| request_sha256 != request || semantic_sha256 != semantic)
    {
        return Err(failure(
            "compile receipt belongs to another context or version",
        ));
    }
    let source_path = PathBuf::from(string(&header[4])?);
    let snapshot = PathBuf::from(string(&header[6])?);
    if !source_path.is_absolute()
        || !snapshot.is_absolute()
        || snapshot
            != path
                .parent()
                .ok_or_else(|| failure("compile receipt has no owner directory"))?
                .join("source.hs")
    {
        return Err(failure("compile source snapshot has another owner"));
    }
    let source_bytes =
        crate::certified_products::read_bounded_with_operation(&snapshot, 32 << 20, operation)
            .map_err(compiler_evidence_failure)?;
    operation
        .charge(source_bytes.len())
        .map_err(|error| compiler_evidence_failure(error.into()))?;
    let source_sha256: [u8; 32] = sha2::Sha256::digest(&source_bytes).into();
    if string(&header[5])? != hex(&source_sha256) {
        return Err(failure("compile source snapshot changed"));
    }
    let source = std::str::from_utf8(&source_bytes)
        .map_err(failure)?
        .to_owned();
    let evidence_bytes = string(&header[7])?.as_bytes().to_vec();
    operation
        .charge(
            evidence_bytes
                .len()
                .checked_mul(32)
                .ok_or_else(|| failure("source evidence accounting overflow"))?,
        )
        .map_err(|error| compiler_evidence_failure(error.into()))?;
    let evidence: crate::cache::DependencyEvidence =
        serde_json::from_slice(&evidence_bytes).map_err(failure)?;
    if evidence.version != 4 {
        return Err(failure("compile receipt dependency evidence version"));
    }
    let edges = list(&header[8], 4096)?
        .iter()
        .map(|module| {
            let module = row(module, 4)?;
            let owner = identity(string(&module[0])?, string(&module[1])?);
            let boot = boolean(&module[2])?;
            let imports = list(&module[3], 4096)?
                .iter()
                .map(|edge| {
                    let edge = row(edge, 4)?;
                    Ok(ExactReceiptImport {
                        qualifier: crate::cache::ImportQualifier::try_from(
                            string(&edge[0])?.to_owned(),
                        )
                        .map_err(failure)?,
                        module: string(&edge[1])?.to_owned(),
                        boot: boolean(&edge[2])?,
                        unit: string(&edge[3])?.to_owned(),
                    })
                })
                .collect::<Result<Vec<_>, CompileError>>()?;
            Ok(ExactReceiptModule {
                owner,
                boot,
                imports,
            })
        })
        .collect::<Result<Vec<_>, CompileError>>()?;
    let source_selection = row(&header[9], 2)?;
    let claims = list(&source_selection[0], 4096)?
        .iter()
        .map(|claim| {
            let claim = row(claim, 5)?;
            Ok(crate::execution_source::SourceSelectedOriginalClaim {
                owner: identity(string(&claim[0])?, string(&claim[1])?),
                certificate_sha256: crate::execution_source::parse_digest(string(&claim[2])?)?,
                interface_sha256: crate::execution_source::parse_digest(string(&claim[3])?)?,
                source_sha256: crate::execution_source::parse_digest(string(&claim[4])?)?,
            })
        })
        .collect::<Result<Vec<_>, CompileError>>()?;
    let selection_evidence = if claims.is_empty() {
        if source_selection[1] != Value::Null {
            return Err(failure(
                "empty original selection has nonempty source evidence",
            ));
        }
        None
    } else {
        let json = string(&source_selection[1])?;
        operation
            .charge(
                json.len()
                    .checked_mul(32)
                    .ok_or_else(|| failure("selection evidence accounting overflow"))?,
            )
            .map_err(|error| compiler_evidence_failure(error.into()))?;
        Some(serde_json::from_str(json).map_err(failure)?)
    };
    Ok(DecodedExactCompilationReceipt {
        source_path,
        source_sha256,
        source,
        evidence_bytes,
        evidence,
        edges,
        claims,
        selection_evidence,
    })
}

fn bounded_read(path: &Path, limit: u64) -> Result<Vec<u8>, CompileError> {
    use std::io::Read;
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .map_err(|error| failure(format!("exact artifact {}: {error}", path.display())))?
        .take(limit + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(failure("exact artifact exceeds byte bound"));
    }
    Ok(bytes)
}

fn retained_generation_inputs<'a>(
    policy: RetainedGenerationPolicy,
    retained: BTreeMap<tidepool_extract_cmd::SymbolIdentity, u64>,
    imports: impl Iterator<Item = &'a crate::certified_products::PendingImportOwner>,
) -> Result<BTreeMap<tidepool_extract_cmd::SymbolIdentity, u64>, CompileError> {
    match policy {
        RetainedGenerationPolicy::PreserveCertifiedDemand => {
            certified_retained_generation_tags(retained, imports)
        }
        RetainedGenerationPolicy::PureActivationPreview => {
            if !retained.is_empty() {
                return Err(failure(
                    "pure activation preview cannot carry retained heap generations",
                ));
            }
            Ok(BTreeMap::new())
        }
    }
}

fn certified_retained_generation_tags<'a>(
    mut retained: BTreeMap<tidepool_extract_cmd::SymbolIdentity, u64>,
    imports: impl Iterator<Item = &'a crate::certified_products::PendingImportOwner>,
) -> Result<BTreeMap<tidepool_extract_cmd::SymbolIdentity, u64>, CompileError> {
    use crate::certified_products::PendingImportOwner;
    for import in imports {
        let (identity, generation) = match import {
            PendingImportOwner::Retained {
                identity,
                generation,
            } => (identity, generation),
            PendingImportOwner::RetainedPackage {
                binder, generation, ..
            } => (binder, generation),
            PendingImportOwner::Source { .. } | PendingImportOwner::Package { .. } => continue,
        };
        let identity = tidepool_extract_cmd::SymbolIdentity {
            unit: identity.unit.clone(),
            module: identity.module.clone(),
            namespace: identity.namespace.clone(),
            occurrence: identity.occurrence.clone(),
            record_parent: identity.record_parent.clone(),
        };
        if retained
            .insert(identity, *generation)
            .is_some_and(|old| old != *generation)
        {
            return Err(failure(
                "certified native demand has conflicting retained generations",
            ));
        }
    }
    Ok(retained)
}
fn row(value: &Value, length: usize) -> Result<&[Value], CompileError> {
    let values = list(value, length)?;
    if values.len() != length {
        return Err(failure("invalid exact compile row"));
    }
    Ok(values)
}
fn list(value: &Value, limit: usize) -> Result<&[Value], CompileError> {
    match value {
        Value::Array(values) if values.len() <= limit => Ok(values),
        _ => Err(failure("invalid exact compile inventory")),
    }
}
fn string(value: &Value) -> Result<&str, CompileError> {
    match value {
        Value::Text(value) => Ok(value),
        _ => Err(failure("invalid exact compile text")),
    }
}
fn boolean(value: &Value) -> Result<bool, CompileError> {
    match value {
        Value::Bool(value) => Ok(*value),
        _ => Err(failure("invalid exact compile boolean")),
    }
}

fn scope_interface_evidence(
    entry: &ArtifactEntry,
    root: &Path,
    validation: &mut PackageInterfaceValidation,
) -> Result<Value, CompileError> {
    if entry.descriptor.kind != entry.payload.artifact_kind() {
        return Err(failure("interface evidence kind differs from payload"));
    }
    let interface = match &entry.payload {
        ArtifactPayload::Canonical(interface) => interface,
        ArtifactPayload::Original(product) => product
            .module_interface()
            .ok_or_else(|| failure("native interface evidence is missing"))?,
        ArtifactPayload::Interface(_, role) => {
            let tag = match role {
                JoinedInterfaceRole::LexicalJoin => "join",
                JoinedInterfaceRole::ValueInterface => "value",
            };
            return Ok(Value::Array(vec![text(tag)]));
        }
    };
    if interface.unit() != entry.descriptor.owner.unit
        || interface.module() != entry.descriptor.owner.module
        || interface.producer_sha256() != entry.descriptor.producer_sha256
        || interface.interface_sha256() != entry.descriptor.interface_sha256
        || interface.package_imports_sha256() != entry.descriptor.package_imports_sha256
    {
        return Err(failure(
            "module evidence differs from its selected artifact",
        ));
    }
    let reference = recovery_artifacts::materialize_module_interface(
        root,
        interface,
        validation,
        MaterializationMode::Scratch,
    )
    .map_err(failure)?;
    Ok(Value::Array(vec![
        text(match interface.origin() {
            crate::certified_products::CanonicalOrigin::SourceOriginal { .. } => "module",
            crate::certified_products::CanonicalOrigin::NativeAuthoredDeclaration { .. } => {
                "native-declaration"
            }
        }),
        path_value(&root.join(reference.certificate_path))?,
        text(hex(&reference.certificate_sha256)),
        reference
            .core
            .as_ref()
            .map(|core| path_value(&root.join(&core.path)))
            .transpose()?
            .unwrap_or(Value::Null),
        reference
            .core
            .as_ref()
            .map_or(Value::Null, |core| text(hex(&core.sha256))),
    ]))
}

fn failure(message: impl std::fmt::Display) -> CompileError {
    CompileError::ExtractFailed(format!("exact declaration context: {message}"))
}

fn compiler_evidence_failure(error: crate::certified_products::CertificationError) -> CompileError {
    CompileError::CompilerEvidence(Box::new(error))
}

fn identity(unit: &str, module: &str) -> ExactModuleIdentity {
    ExactModuleIdentity {
        unit: unit.to_owned(),
        module: module.to_owned(),
    }
}

impl ExactDeclarationContext {
    pub fn new(
        authored: &[Arc<CertifiedAuthoredDeclaration>],
        joins: &[Arc<AcceptedJoin>],
        lexical: Vec<ExactLexicalNode>,
    ) -> Result<Self, CompileError> {
        Self {
            producer: [0; 32],
            inventory: ArtifactInventory::default().empty_view(),
            lexical: Vec::new(),
            original_instance_environment: OriginalInstanceEnvironment::Unknown,
            template_imports: None,
        }
        .extend(authored, joins, lexical)
    }

    /// Admit original references with their embedded canonical proofs, plus
    /// separately supplied canonical type owners and joined interfaces.
    pub fn capture_recovery(
        root: &Path,
        products: &[RecoveryArtifactRef],
        module_interfaces: &[recovery_artifacts::RecoveryModuleInterfaceRef],
        joins: &[RecoveryJoinRef],
        lexical: Vec<ExactLexicalNode>,
    ) -> Result<Self, CompileError> {
        Self::capture_recovery_with_value_interfaces(
            root,
            products,
            module_interfaces,
            joins,
            &[],
            lexical,
        )
    }

    pub fn capture_recovery_with_value_interfaces(
        root: &Path,
        products: &[RecoveryArtifactRef],
        module_interfaces: &[recovery_artifacts::RecoveryModuleInterfaceRef],
        joins: &[RecoveryJoinRef],
        values: &[RecoveryValueInterfaceRef],
        lexical: Vec<ExactLexicalNode>,
    ) -> Result<Self, CompileError> {
        Self::capture_recovery_inputs(
            root,
            products,
            module_interfaces,
            joins,
            values,
            None,
            lexical,
        )
    }
    pub fn capture_recovery_with_inventory(
        root: &Path,
        products: &[RecoveryArtifactRef],
        module_interfaces: &[recovery_artifacts::RecoveryModuleInterfaceRef],
        joins: &[RecoveryJoinRef],
        values: &[RecoveryValueInterfaceRef],
        descriptors: &[crate::artifact_inventory::ArtifactDescriptor],
        dependencies: &[(
            crate::artifact_inventory::ArtifactId,
            crate::artifact_inventory::ArtifactId,
            crate::artifact_inventory::ArtifactDependency,
        )],
        native_groups: &[crate::artifact_inventory::NativeGroupKey],
        lexical: Vec<ExactLexicalNode>,
    ) -> Result<Self, CompileError> {
        let inventory = RecoveredArtifactInventory::capture_inputs(
            root,
            products,
            module_interfaces,
            joins,
            values,
            Some((descriptors, dependencies)),
        )
        .map_err(failure)?;
        inventory.context(
            &inventory.entries.keys().copied().collect::<Vec<_>>(),
            native_groups,
            lexical,
        )
    }
    fn capture_recovery_inputs(
        root: &Path,
        products: &[RecoveryArtifactRef],
        module_interfaces: &[recovery_artifacts::RecoveryModuleInterfaceRef],
        joins: &[RecoveryJoinRef],
        values: &[RecoveryValueInterfaceRef],
        inventory: Option<(
            &[crate::artifact_inventory::ArtifactDescriptor],
            &[(
                crate::artifact_inventory::ArtifactId,
                crate::artifact_inventory::ArtifactId,
                crate::artifact_inventory::ArtifactDependency,
            )],
        )>,
        lexical: Vec<ExactLexicalNode>,
    ) -> Result<Self, CompileError> {
        let inventory = RecoveredArtifactInventory::capture_inputs(
            root,
            products,
            module_interfaces,
            joins,
            values,
            inventory,
        )
        .map_err(failure)?;
        inventory.context_all_groups(
            &inventory.entries.keys().copied().collect::<Vec<_>>(),
            lexical,
        )
    }

    /// Merge recovered inputs with fresh certificates, replacing the complete
    /// selected lexical graph. One owner cannot acquire two implementations.
    pub fn extend(
        mut self,
        authored: &[Arc<CertifiedAuthoredDeclaration>],
        joins: &[Arc<AcceptedJoin>],
        lexical: Vec<ExactLexicalNode>,
    ) -> Result<Self, CompileError> {
        let mut entries = Vec::new();
        for certificate in authored {
            self.admit_producer(certificate.toolchain_identity_sha256())?;
            self.inventory = self.inventory.merge(certificate.artifact_view())?;
        }
        for join in joins {
            self.admit_producer(join.toolchain_identity_sha256())?;
            self.inventory = self.inventory.merge(join.context().artifact_view())?;
            entries.push(ArtifactEntry::interface(
                join.interface().clone(),
                JoinedInterfaceRole::LexicalJoin,
                join.context()
                    .interface_owners()
                    .into_iter()
                    .map(|interface| interface.owner)
                    .collect(),
            ));
        }
        self.inventory = self.inventory.inventory().admit(&self.inventory, entries)?;
        self.lexical = lexical;
        self.original_instance_environment = OriginalInstanceEnvironment::Unknown;
        self.normalize()?;
        Ok(self)
    }

    /// Add issued joins and their selected lexical nodes without duplicating
    /// an already retained owner. Equal import sets are idempotent; different
    /// selections for the same owner remain a conflict.
    pub fn extend_lexical_joins(
        self,
        joins: &[Arc<AcceptedJoin>],
        lexical: &[ExactLexicalNode],
    ) -> Result<Self, CompileError> {
        let lexical = compose_lexical_nodes(self.lexical.iter().chain(lexical))?;
        self.extend(&[], joins, lexical)
    }

    /// Admit the original supporting homes sealed by the same checked-cell
    /// transaction. Lexical exposure remains a separate caller-selected graph.
    pub(crate) fn extend_checked_original_products(
        mut self,
        producer_sha256: [u8; 32],
        products: &[CertifiedRecoveryProduct],
    ) -> Result<Self, CompileError> {
        self.admit_producer(producer_sha256)?;
        if products.is_empty() {
            return Ok(self);
        }
        let existing = self.inventory.entries_for_owners(
            products
                .iter()
                .map(|product| identity(&product.owner().unit, &product.owner().module)),
        )?;
        let mut entries = Vec::new();
        let mut validation = PackageInterfaceValidation::default();
        for product in products {
            let owner = identity(&product.owner().unit, &product.owner().module);
            if let Some(entry) = existing.get(&owner) {
                if let ArtifactPayload::Canonical(interface) = &entry.payload {
                    if product.module_interface() != Some(interface) {
                        return Err(failure("supporting original differs from canonical module"));
                    }
                    entries.push(ArtifactEntry::original_with_validation(
                        producer_sha256,
                        product.clone(),
                        &mut validation,
                    )?);
                    continue;
                }
                let ArtifactPayload::Original(previous) = &entry.payload else {
                    return Err(failure(
                        "supporting original collides with synthetic interface",
                    ));
                };
                if previous.owner() != product.owner()
                    || previous.interface_bytes() != product.interface_bytes()
                    || previous.product_bytes() != product.product_bytes()
                    || previous.package_imports_bytes() != product.package_imports_bytes()
                    || previous.certification_bytes() != product.certification_bytes()
                {
                    return Err(failure("supporting original differs from retained owner"));
                }
                continue;
            }
            entries.push(ArtifactEntry::original_with_validation(
                producer_sha256,
                product.clone(),
                &mut validation,
            )?);
        }
        self.inventory = self.inventory.inventory().admit(&self.inventory, entries)?;
        Ok(self)
    }

    pub fn toolchain_identity_sha256(&self) -> [u8; 32] {
        self.producer
    }
    pub fn artifact_view(&self) -> &ArtifactView {
        &self.inventory
    }
    /// Merge retained interface custody without granting authored imports.
    pub fn extend_interface_artifacts(
        mut self,
        artifacts: &ArtifactView,
    ) -> Result<Self, CompileError> {
        for descriptor in artifacts.descriptors() {
            self.admit_producer(descriptor.producer_sha256)?;
        }
        let owners = artifacts
            .interface_owners()
            .into_iter()
            .map(|interface| interface.owner)
            .collect::<Vec<_>>();
        let interfaces = artifacts.interface_projection(&owners)?;
        self.inventory = self.inventory.merge(&interfaces)?;
        self.normalize()?;
        Ok(self)
    }

    /// Retain the interface evidence admitted by an original compiler proof.
    /// Only compiler-owned issuers may supply the producer identity.
    pub(crate) fn from_authenticated_interfaces(
        producer: [u8; 32],
        artifacts: &ArtifactView,
    ) -> Result<Self, CompileError> {
        let mut context = Self::new(&[], &[], Vec::new())?;
        context.admit_producer(producer)?;
        context.extend_interface_artifacts(artifacts)
    }

    /// Retain already authenticated original code owners and their original
    /// lexical graph. Only the original compiler output proof issues this.
    pub(crate) fn from_authenticated_execution(
        producer: [u8; 32],
        artifacts: &ArtifactView,
        lexical: Vec<ExactLexicalNode>,
        target: ExactModuleIdentity,
        required_instance_owners: &[ExactModuleIdentity],
    ) -> Result<Self, CompileError> {
        let mut context = Self::new(&[], &[], Vec::new())?;
        context.admit_producer(producer)?;
        for descriptor in artifacts.descriptors() {
            context.admit_producer(descriptor.producer_sha256)?;
        }
        context.inventory = context.inventory.merge(artifacts)?;
        context.lexical = lexical;
        let interfaces = context
            .interface_owners()
            .into_iter()
            .map(|row| row.owner)
            .collect::<BTreeSet<_>>();
        let selected = context
            .lexical
            .iter()
            .map(|row| &row.owner)
            .collect::<BTreeSet<_>>();
        let canonical_target = context.inventory.entries().iter().any(|entry| {
            entry.descriptor.owner == target
                && matches!(
                    entry.payload,
                    ArtifactPayload::Canonical(_) | ArtifactPayload::Original(_)
                )
        });
        let missing = required_instance_owners
            .iter()
            .chain(std::iter::once(&target))
            .filter(|owner| !interfaces.contains(*owner) || !selected.contains(owner))
            .cloned()
            .chain((!canonical_target).then_some(target.clone()))
            .chain(context.lexical.iter().flat_map(|node| {
                std::iter::once(&node.owner)
                    .chain(node.imports.iter())
                    .filter(|owner| !interfaces.contains(*owner) || !selected.contains(owner))
                    .cloned()
            }))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        // A consumed owner without a retained interface is explicit missing
        // evidence, not a selected lexical interface or a negative instance
        // result. Retain only the usable graph and commit the missing census.
        context
            .lexical
            .retain(|node| interfaces.contains(&node.owner));
        let retained = context
            .lexical
            .iter()
            .map(|node| node.owner.clone())
            .collect::<BTreeSet<_>>();
        for node in &mut context.lexical {
            node.imports.retain(|owner| retained.contains(owner));
        }
        context.normalize()?;
        context.original_instance_environment = if !missing.is_empty() {
            OriginalInstanceEnvironment::MissingOriginalOwners(missing)
        } else {
            OriginalInstanceEnvironment::Complete { target }
        };
        Ok(context)
    }

    pub(crate) fn original_instance_environment(&self) -> &OriginalInstanceEnvironment {
        &self.original_instance_environment
    }

    /// The original compiler target owns the orphan import census in its
    /// authenticated canonical interface. Retention alone does not grant it.
    pub(crate) fn original_instance_target(&self) -> Result<&ExactModuleIdentity, CompileError> {
        match &self.original_instance_environment {
            OriginalInstanceEnvironment::Complete { target } => Ok(target),
            _ => Err(failure(
                "activation preview lacks its original target interface",
            )),
        }
    }

    /// Select interface custody without losing the original producer when the
    /// selected type closure contains only package Names.
    pub fn select_interface_roots(
        &self,
        roots: Vec<crate::artifact_inventory::ArtifactId>,
    ) -> Result<Self, CompileError> {
        let selected = self.inventory.select_roots(roots)?;
        Self::from_authenticated_interfaces(self.producer, &selected)
    }

    /// Merge original type-interface evidence without granting lexical imports.
    /// Even an empty home closure must agree on its compiler producer.
    pub fn extend_interface_context(mut self, context: &Self) -> Result<Self, CompileError> {
        if context.producer != [0; 32] {
            self.admit_producer(context.producer)?;
        }
        self.extend_interface_artifacts(context.artifact_view())
    }

    /// Project only interfaces selected by the original checked template. The
    /// immutable context owns their seals; template text selects imports, never
    /// manufactures interface or native authority.
    #[cfg(test)]
    fn template_interface_graph(
        &self,
        templates: &[String],
    ) -> Result<BTreeMap<ExactModuleIdentity, TemplateInterfaceNode>, CompileError> {
        Ok(self.selected_template_imports(templates)?.graph)
    }

    pub(crate) fn selected_template_imports(
        &self,
        templates: &[String],
    ) -> Result<SelectedTemplateImports, CompileError> {
        let mut roots = self
            .lexical_graph()
            .iter()
            .filter(|node| template_selects_owner(templates, &node.owner))
            .map(|node| node.owner.clone())
            .collect::<BTreeSet<_>>();
        let mut graph = self.interface_graph_for_roots(roots.iter().cloned().collect())?;
        if let Some(retained) = &self.template_imports {
            retained.validate(self.producer, self.artifact_view())?;
            let selected = retained
                .roots
                .iter()
                .filter(|owner| template_selects_owner(templates, owner))
                .cloned()
                .collect::<BTreeSet<_>>();
            let mut pending = selected.iter().cloned().collect::<Vec<_>>();
            let mut seen = BTreeSet::new();
            while let Some(owner) = pending.pop() {
                if !seen.insert(owner.clone()) {
                    continue;
                }
                let node = &retained.graph[&owner].interface;
                if graph
                    .insert(owner.clone(), node.clone())
                    .is_some_and(|old| old != *node)
                {
                    return Err(failure(
                        "template and lexical closures select different interfaces",
                    ));
                }
                pending.extend(node.imports.iter().cloned());
            }
            roots.extend(selected);
        }
        Ok(SelectedTemplateImports { roots, graph })
    }

    /// A pure preview observes every instance visible to the original request,
    /// including orphan instances unrelated to the input's nominal type owner.
    /// This compiler-issued context grants graph edges, never authored imports.
    pub(crate) fn original_preview_interface_graph(
        &self,
    ) -> Result<BTreeMap<ExactModuleIdentity, TemplateInterfaceNode>, CompileError> {
        let target = self.original_instance_target()?;
        if self
            .lexical_graph()
            .iter()
            .any(|node| node.owner.unit != "main")
        {
            return Err(failure(
                "activation preview original graph belongs to another home unit",
            ));
        }
        self.normalize()?;
        let graph = self.interface_graph_for_roots(
            self.lexical_graph()
                .iter()
                .map(|node| node.owner.clone())
                .collect(),
        )?;
        if !graph.contains_key(target) {
            return Err(failure(
                "activation preview original target is outside its sealed graph",
            ));
        }
        Ok(graph)
    }

    fn interface_graph_for_roots(
        &self,
        mut pending: Vec<ExactModuleIdentity>,
    ) -> Result<BTreeMap<ExactModuleIdentity, TemplateInterfaceNode>, CompileError> {
        let entries = self.artifact_view().entries();
        let lexical = self
            .lexical_graph()
            .iter()
            .map(|node| (&node.owner, node))
            .collect::<BTreeMap<_, _>>();
        let mut selected = BTreeMap::new();
        while let Some(owner) = pending.pop() {
            if selected.contains_key(&owner) {
                continue;
            }
            let node = lexical
                .get(&owner)
                .ok_or_else(|| failure("checked template lexical closure is incomplete"))?;
            let seals = entries
                .iter()
                .filter(|entry| entry.descriptor.owner == owner)
                .map(|entry| entry.descriptor.interface_sha256)
                .collect::<BTreeSet<_>>();
            if seals.len() != 1 {
                return Err(failure(
                    "checked template selection lacks one exact interface seal",
                ));
            }
            pending.extend(node.imports.iter().cloned());
            selected.insert(
                owner,
                TemplateInterfaceNode {
                    interface_sha256: *seals
                        .iter()
                        .next()
                        .ok_or_else(|| failure("missing template interface seal"))?,
                    imports: node.imports.clone(),
                },
            );
        }
        Ok(selected)
    }

    pub(crate) fn interface_owners(&self) -> Vec<ExactInterfaceOwner> {
        self.inventory.interface_owners()
    }
    pub(crate) fn module_interfaces(
        &self,
    ) -> Vec<crate::certified_products::CertifiedModuleInterface> {
        self.inventory
            .entries()
            .iter()
            .filter_map(|entry| match &entry.payload {
                ArtifactPayload::Canonical(interface) => Some(interface.clone()),
                _ => None,
            })
            .collect()
    }

    pub fn materialize_module_interfaces(
        &self,
        root: &Path,
    ) -> Result<Vec<recovery_artifacts::RecoveryModuleInterfaceRef>, CompileError> {
        let mut validation = PackageInterfaceValidation::default();
        self.module_interfaces()
            .iter()
            .map(|interface| {
                recovery_artifacts::materialize_module_interface(
                    root,
                    interface,
                    &mut validation,
                    MaterializationMode::Durable,
                )
                .map_err(failure)
            })
            .collect()
    }

    /// Select the original native owner from its compiler-issued origin. A
    /// lexical projection may have a different owner and grants no native root.
    pub fn authored_native_root(
        &self,
        generation: u64,
    ) -> Result<crate::artifact_inventory::ArtifactId, CompileError> {
        use crate::certified_products::CanonicalOrigin;
        let entries = self.inventory.entries();
        let roots = entries
            .iter()
            .filter(|entry| match &entry.payload {
                ArtifactPayload::Original(product) if generation != 0 => {
                    product
                        .module_interface()
                        .map(|interface| interface.origin())
                        == Some(CanonicalOrigin::NativeAuthoredDeclaration { generation })
                }
                _ => false,
            })
            .map(|entry| entry.descriptor.id)
            .collect::<Vec<_>>();
        match roots.as_slice() {
            [root] => Ok(*root),
            _ => Err(admission_failure(
                ArtifactInventoryFailure::AuthoredNativeRoot {
                    generation,
                    found: roots.len(),
                },
            )),
        }
    }

    pub fn recovery_products(&self) -> Vec<CertifiedRecoveryProduct> {
        self.inventory
            .entries()
            .iter()
            .filter_map(|entry| match &entry.payload {
                ArtifactPayload::Original(product) => Some(product.clone()),
                _ => None,
            })
            .collect()
    }
    pub fn joined_interfaces(&self) -> Vec<CertifiedJoinedInterface> {
        self.inventory
            .entries()
            .iter()
            .filter_map(|entry| match &entry.payload {
                ArtifactPayload::Interface(interface, JoinedInterfaceRole::LexicalJoin) => {
                    Some(interface.clone())
                }
                ArtifactPayload::Interface(_, JoinedInterfaceRole::ValueInterface)
                | ArtifactPayload::Original(_)
                | ArtifactPayload::Canonical(_) => None,
            })
            .collect()
    }
    pub fn lexical_graph(&self) -> &[ExactLexicalNode] {
        &self.lexical
    }

    pub fn value_interfaces(&self) -> Vec<CertifiedValueInterface> {
        self.inventory
            .entries()
            .iter()
            .filter_map(|entry| match &entry.payload {
                ArtifactPayload::Interface(interface, JoinedInterfaceRole::ValueInterface) => {
                    Some(CertifiedValueInterface::from_admitted_interface(
                        interface.clone(),
                        entry.requirements.clone(),
                    ))
                }
                ArtifactPayload::Interface(_, JoinedInterfaceRole::LexicalJoin)
                | ArtifactPayload::Original(_)
                | ArtifactPayload::Canonical(_) => None,
            })
            .collect()
    }
    pub fn extend_with_value_interfaces(
        mut self,
        values: &[Arc<CertifiedValueInterface>],
        lexical: Vec<ExactLexicalNode>,
    ) -> Result<Self, CompileError> {
        let mut entries = Vec::new();
        for value in values {
            self.admit_producer(value.interface().toolchain_identity_sha256())?;
            entries.push(ArtifactEntry::interface(
                value.interface().clone(),
                JoinedInterfaceRole::ValueInterface,
                value.requirements().to_vec(),
            ));
        }
        self.inventory = self.inventory.inventory().admit(&self.inventory, entries)?;
        self.lexical = lexical;
        self.original_instance_environment = OriginalInstanceEnvironment::Unknown;
        self.normalize()?;
        Ok(self)
    }

    pub(crate) fn extend_retained_value_artifacts(
        mut self,
        values: &[Arc<crate::checked_cell::CheckedValueArtifact>],
    ) -> Result<Self, CompileError> {
        let mut lexical = self.lexical.clone();
        for value in values {
            let interface = value.certified_interface();
            self.admit_producer(interface.interface().toolchain_identity_sha256())?;
            self.inventory = self.inventory.merge(value.artifact_view())?;
            lexical.extend_from_slice(value.source_lexical());
            if let Some(retained) = value.template_imports() {
                self.inventory = self.inventory.merge(&retained.artifacts)?;
                self.template_imports = Some(match &self.template_imports {
                    Some(existing) => RetainedTemplateImports::merge(existing, retained)?,
                    None => retained.clone(),
                });
            }
        }
        self.lexical = compose_lexical_nodes(&lexical)?;
        self.original_instance_environment = OriginalInstanceEnvironment::Unknown;
        self.normalize()?;
        Ok(self)
    }

    pub(crate) fn retain_value_source_surface(
        &self,
        value: &ArtifactView,
        support: &[ExactLexicalNode],
    ) -> Result<(ArtifactView, Vec<ExactLexicalNode>), CompileError> {
        let retained = value
            .descriptors()
            .into_iter()
            .map(|descriptor| descriptor.owner)
            .collect::<BTreeSet<_>>();
        let selected = compose_lexical_nodes(
            self.lexical
                .iter()
                .chain(support)
                .filter(|node| !node.owner.module.starts_with("Tidepool.Session.")),
        )?
        .into_iter()
        .map(|node| (node.owner, node.imports))
        .collect::<BTreeMap<_, _>>();
        let roots = selected
            .keys()
            .filter(|owner| retained.contains(*owner))
            .cloned()
            .collect::<Vec<_>>();
        let implementations = self.artifact_view().source_implementation_roles();
        let lexical = crate::declaration_join::source_lexical_surface(
            &roots,
            &selected,
            &[],
            &implementations,
        )?
        .lexical;
        let entries = self
            .artifact_view()
            .entries_for_owners(lexical.iter().map(|node| node.owner.clone()))?;
        if entries.len() != lexical.len() {
            return Err(failure(
                "retained source surface lacks its exact artifact owner",
            ));
        }
        let view = self.artifact_view().select_roots(
            value
                .root_entries()
                .iter()
                .map(|entry| entry.descriptor.id)
                .chain(entries.values().map(|entry| entry.descriptor.id))
                // Original executable availability is retained independently
                // of the lexical surface above. Type-only projection remains
                // an explicit operation that removes native authority.
                .chain(self.artifact_view().entries().iter().filter_map(|entry| {
                    matches!(entry.payload, ArtifactPayload::Original(_))
                        .then_some(entry.descriptor.id)
                }))
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect(),
        )?;
        Ok((view, lexical))
    }

    pub(crate) fn extend_program_value_interface(
        self,
        value: Arc<CertifiedValueInterface>,
    ) -> Result<Self, CompileError> {
        let selected = self
            .lexical
            .iter()
            .map(|node| node.owner.clone())
            .collect::<BTreeSet<_>>();
        let mut lexical = self.lexical.clone();
        lexical.push(ExactLexicalNode {
            owner: identity(value.interface().unit(), value.interface().module()),
            // Type-owner evidence retains hydration dependencies without
            // granting their names or instances public lexical selection.
            imports: value
                .requirements()
                .iter()
                .filter(|owner| selected.contains(*owner))
                .cloned()
                .collect(),
        });
        self.extend_with_value_interfaces(&[value], lexical)
    }

    /// Logical identity is independent of materialization paths and of the
    /// optional source bytes retained only by a fresh authored certificate.
    pub fn semantic_sha256(&self) -> [u8; 32] {
        self.semantic_sha256_from_metadata(&self.inventory.metadata_snapshot())
    }

    fn semantic_sha256_from_metadata(&self, metadata: &ArtifactMetadataSnapshot) -> [u8; 32] {
        use sha2::Digest;
        let mut lexical = self.lexical.iter().collect::<Vec<_>>();
        lexical.sort_by_key(|node| &node.owner);
        let mut native = metadata
            .artifacts
            .values()
            .filter_map(|entry| match &entry.payload {
                ArtifactPayload::Original(product) => Some((&entry.descriptor, product)),
                _ => None,
            })
            .collect::<Vec<_>>();
        native.sort_by_key(|(descriptor, _)| (&descriptor.owner, descriptor.id));
        let mut fields = vec![
            text("TPEXACTCONTEXT"),
            text(
                if matches!(
                    self.original_instance_environment,
                    OriginalInstanceEnvironment::Unknown
                ) {
                    "2"
                } else {
                    "4"
                },
            ),
            text(hex(&sha2::Sha256::digest(
                serde_json::to_vec(&(
                    metadata.descriptors(),
                    metadata.dependencies(),
                    &metadata.selected_native_groups,
                ))
                .expect("inventory encoding"),
            )
            .into())),
            text(hex(&self.producer)),
            Value::Array(
                native
                    .into_iter()
                    .map(|(descriptor, product)| {
                        let owner = product.owner();
                        Value::Array(vec![
                            text(&owner.unit),
                            text(&owner.module),
                            text(hex(&owner.module_version.0)),
                            text(hex(&owner.skinny_iface_sha256)),
                            text(hex(&owner.product_sha256)),
                            text(hex(&descriptor.package_imports_sha256)),
                            text(hex(&descriptor
                                .certification_sha256
                                .expect("original certification digest"))),
                        ])
                    })
                    .collect(),
            ),
            Value::Array(
                metadata
                    .entries
                    .values()
                    .filter_map(|entry| match &entry.payload {
                        ArtifactPayload::Interface(interface, JoinedInterfaceRole::LexicalJoin) => {
                            Some((&entry.descriptor, interface))
                        }
                        ArtifactPayload::Interface(_, JoinedInterfaceRole::ValueInterface)
                        | ArtifactPayload::Original(_)
                        | ArtifactPayload::Canonical(_) => None,
                    })
                    .map(|(descriptor, join)| {
                        Value::Array(vec![
                            text(join.unit()),
                            text(join.module()),
                            text(hex(&descriptor.interface_sha256)),
                            text(hex(&descriptor.package_imports_sha256)),
                        ])
                    })
                    .collect(),
            ),
            Value::Array(
                lexical
                    .into_iter()
                    .map(|node| {
                        let mut imports = node.imports.iter().collect::<Vec<_>>();
                        imports.sort();
                        Value::Array(vec![
                            module_value(&node.owner),
                            Value::Array(imports.into_iter().map(module_value).collect()),
                        ])
                    })
                    .collect(),
            ),
        ];
        match &self.original_instance_environment {
            OriginalInstanceEnvironment::Unknown => {}
            OriginalInstanceEnvironment::Complete { target } => fields.push(Value::Array(vec![
                text("original-instances-complete"),
                module_value(target),
            ])),
            OriginalInstanceEnvironment::MissingOriginalOwners(owners) => {
                fields.push(Value::Array(vec![
                    text("original-instances-missing"),
                    Value::Array(owners.iter().map(module_value).collect()),
                ]))
            }
        }
        if let Some(retained) = &self.template_imports {
            fields.push(Value::Array(vec![
                text("retained-template-imports1"),
                Value::Array(retained.roots.iter().map(module_value).collect()),
                Value::Array(
                    retained
                        .graph
                        .iter()
                        .map(|(owner, node)| {
                            Value::Array(vec![
                                module_value(owner),
                                text(hex(&node.canonical.0)),
                                text(hex(&node.interface.interface_sha256)),
                                Value::Array(
                                    node.interface.imports.iter().map(module_value).collect(),
                                ),
                            ])
                        })
                        .collect(),
                ),
            ]));
        }
        let value = Value::Array(fields);
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(&value, &mut bytes).expect("owned value encoding");
        sha2::Sha256::digest(bytes).into()
    }

    pub(crate) fn validate_artifacts(
        &self,
        artifacts: &[DeclarationArtifact],
    ) -> Result<(), CompileError> {
        self.validate_artifacts_from_metadata(artifacts, &self.inventory.metadata_snapshot())
    }

    fn validate_artifacts_from_metadata(
        &self,
        artifacts: &[DeclarationArtifact],
        metadata: &ArtifactMetadataSnapshot,
    ) -> Result<(), CompileError> {
        let retained = self.inventory.retained_materialization();
        let rows = retained
            .as_ref()
            .map(|retained| retained.selected_rows(metadata))
            .unwrap_or_default();
        self.validate_artifacts_with_owned_rows(artifacts, metadata, &rows)
    }

    fn validate_artifacts_with_owned_rows(
        &self,
        artifacts: &[DeclarationArtifact],
        metadata: &ArtifactMetadataSnapshot,
        owned: &BTreeMap<ArtifactId, &RetainedArtifactRow>,
    ) -> Result<(), CompileError> {
        metadata.validate_native_selection()?;
        let mut seen = BTreeSet::new();
        for artifact in artifacts {
            let exact = &artifact.interface;
            let owner = identity(&exact.unit, &exact.module);
            if !seen.insert(owner.clone()) || !exact.path.is_absolute() {
                return Err(failure("duplicate owner or relative interface path"));
            }
            let entry = metadata
                .entries
                .get(&owner)
                .ok_or_else(|| failure("artifact is not owned by the context"))?;
            if exact.requirements.iter().cloned().collect::<BTreeSet<_>>()
                != entry
                    .requirements
                    .iter()
                    .map(|owner| (owner.unit.clone(), owner.module.clone()))
                    .collect()
            {
                return Err(failure("artifact requirements differ from owned context"));
            }
            if entry.descriptor.kind != entry.payload.artifact_kind() {
                return Err(failure("interface evidence kind differs from payload"));
            }
            if owned
                .get(&entry.descriptor.id)
                .is_some_and(|row| row.artifact == *artifact)
            {
                continue;
            }
            let (iface, packages) = match &entry.payload {
                ArtifactPayload::Original(product) => {
                    let snapshot = artifact
                        .product
                        .as_ref()
                        .ok_or_else(|| failure("original product is missing"))?;
                    if !snapshot.path.is_absolute()
                        || snapshot.module != exact.module
                        || snapshot.sha256 != hex(&product.owner().product_sha256)
                        || std::fs::read(&snapshot.path)? != product.product_bytes()
                    {
                        return Err(failure("original product differs from owned bytes"));
                    }
                    (product.interface_bytes(), product.package_imports_bytes())
                }
                ArtifactPayload::Canonical(interface) => {
                    if artifact.product.is_some() {
                        return Err(failure("type interface has an executable product"));
                    }
                    (
                        interface.interface_bytes(),
                        interface.package_imports_bytes(),
                    )
                }
                ArtifactPayload::Interface(join, _) => {
                    if artifact.product.is_some() {
                        return Err(failure("synthetic anchor has an implementation product"));
                    }
                    (join.interface_bytes(), join.package_imports_bytes())
                }
            };
            if sha256(iface) != exact.sha256 || std::fs::read(&exact.path)? != iface {
                return Err(failure("interface differs from owned bytes"));
            }
            let mut packages_path = exact.path.as_os_str().to_os_string();
            packages_path.push(".packages");
            if std::fs::read(std::path::PathBuf::from(packages_path))? != packages {
                return Err(failure("package witness differs from owned bytes"));
            }
        }
        if seen.len() != metadata.entries.len() {
            return Err(failure("owned artifact closure is incomplete"));
        }
        Ok(())
    }

    pub(crate) fn admit_producer(&mut self, producer: [u8; 32]) -> Result<(), CompileError> {
        if producer == [0; 32] || (self.producer != [0; 32] && producer != self.producer) {
            return Err(failure("producer identity differs"));
        }
        self.producer = producer;
        Ok(())
    }

    fn normalize(&self) -> Result<(), CompileError> {
        if let Some(retained) = &self.template_imports {
            retained.validate(self.producer, self.artifact_view())?;
        }
        let interfaces = self.interface_owners();
        let owners = interfaces
            .iter()
            .map(|interface| &interface.owner)
            .collect::<BTreeSet<_>>();
        let lexical_owners = self
            .lexical
            .iter()
            .map(|node| &node.owner)
            .collect::<BTreeSet<_>>();
        let mut seen = BTreeSet::new();
        for node in &self.lexical {
            if !seen.insert(&node.owner) {
                return Err(failure(format!(
                    "duplicate selected lexical owner {}:{}",
                    node.owner.unit, node.owner.module
                )));
            }
            if !owners.contains(&node.owner) {
                return Err(failure(format!(
                    "selected lexical owner lacks its interface {}:{}",
                    node.owner.unit, node.owner.module
                )));
            }
            let mut imports = BTreeSet::new();
            for imported in &node.imports {
                if !imports.insert(imported) {
                    return Err(failure(format!(
                        "duplicate selected lexical import {}:{} -> {}:{}",
                        node.owner.unit, node.owner.module, imported.unit, imported.module
                    )));
                }
                if !lexical_owners.contains(imported) {
                    return Err(failure(format!(
                        "selected lexical edge leaves its graph {}:{} -> {}:{}",
                        node.owner.unit, node.owner.module, imported.unit, imported.module
                    )));
                }
            }
        }
        Ok(())
    }

    pub fn materialize(
        &self,
        root: &Path,
    ) -> Result<MaterializedExactDeclarationContext, CompileError> {
        self.materialize_with_validation(
            root,
            &mut PackageInterfaceValidation::default(),
            MaterializationMode::Durable,
        )
        .map(|(materialized, _)| materialized)
    }

    /// Materialize inspection inputs for the supplied temporary owner. Keep
    /// that owner alive while using the returned paths; recovery publication
    /// continues through the durable `materialize` entry point.
    pub fn materialize_scratch(
        &self,
        directory: &tempfile::TempDir,
    ) -> Result<MaterializedExactDeclarationContext, CompileError> {
        self.materialize_with_validation(
            directory.path(),
            &mut PackageInterfaceValidation::default(),
            MaterializationMode::Scratch,
        )
        .map(|(materialized, _)| materialized)
    }

    fn materialize_with_validation(
        &self,
        root: &Path,
        validation: &mut PackageInterfaceValidation,
        mode: MaterializationMode,
    ) -> Result<
        (
            MaterializedExactDeclarationContext,
            Vec<RecoveryArtifactRef>,
        ),
        CompileError,
    > {
        self.materialize_entries_with_validation(root, &self.inventory.entries(), validation, mode)
    }

    fn materialize_entries_with_validation(
        &self,
        root: &Path,
        entries: &[Arc<ArtifactEntry>],
        validation: &mut PackageInterfaceValidation,
        mode: MaterializationMode,
    ) -> Result<
        (
            MaterializedExactDeclarationContext,
            Vec<RecoveryArtifactRef>,
        ),
        CompileError,
    > {
        let mut native_owners = BTreeSet::new();
        for entry in entries
            .iter()
            .filter(|entry| matches!(entry.payload, ArtifactPayload::Original(_)))
        {
            if !native_owners.insert(entry.descriptor.owner.clone()) {
                return Err(admission_failure(
                    ArtifactInventoryFailure::NativeOwnerAmbiguity {
                        owner: entry.descriptor.owner.clone(),
                    },
                ));
            }
        }
        let products = entries
            .iter()
            .filter_map(|entry| match &entry.payload {
                ArtifactPayload::Original(product) => Some(product.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        let references = if products.is_empty() {
            Vec::new()
        } else {
            recovery_artifacts::materialize_certified_products_with_validation(
                root,
                self.producer,
                &products,
                validation,
                mode,
            )
            .map_err(failure)?
        };
        let native_owners = products
            .iter()
            .map(|product| identity(&product.owner().unit, &product.owner().module))
            .collect::<BTreeSet<_>>();
        let canonical = entries
            .iter()
            .filter_map(|entry| match &entry.payload {
                ArtifactPayload::Canonical(interface)
                    if !native_owners.contains(&entry.descriptor.owner) =>
                {
                    Some(interface)
                }
                _ => None,
            })
            .map(|interface| {
                recovery_artifacts::materialize_module_interface(root, interface, validation, mode)
            })
            .collect::<Result<Vec<_>, _>>()
            .map_err(failure)?;
        let joined = entries
            .iter()
            .filter_map(|entry| match &entry.payload {
                ArtifactPayload::Interface(interface, _) => Some(interface),
                _ => None,
            })
            .map(|interface| interface.materialize_with_validation(root, validation, mode))
            .collect::<Result<Vec<_>, _>>()
            .map_err(failure)?;
        let requirements = entries
            .iter()
            .map(|entry| (&entry.descriptor.owner, &entry.requirements))
            .collect::<BTreeMap<_, _>>();
        let mut artifacts = Vec::new();
        for reference in &references {
            let owner = identity(&reference.unit, &reference.module);
            artifacts.push(DeclarationArtifact {
                interface: ExactIfaceArtifact {
                    unit: reference.unit.clone(),
                    module: reference.module.clone(),
                    path: root.join(&reference.interface_path),
                    sha256: hex(&reference.skinny_iface_sha256),
                    requirements: requirements[&owner]
                        .iter()
                        .map(|owner| (owner.unit.clone(), owner.module.clone()))
                        .collect(),
                },
                product: Some(ModuleSnapshot {
                    module: reference.module.clone(),
                    path: root.join(&reference.product_path),
                    sha256: hex(&reference.product_sha256),
                }),
            });
        }
        for reference in canonical
            .into_iter()
            .map(|reference| reference.interface)
            .chain(joined)
        {
            let owner = identity(&reference.unit, &reference.module);
            artifacts.push(DeclarationArtifact {
                interface: ExactIfaceArtifact {
                    unit: reference.unit,
                    module: reference.module,
                    path: root.join(reference.interface_path),
                    sha256: hex(&reference.skinny_iface_sha256),
                    requirements: requirements[&owner]
                        .iter()
                        .map(|owner| (owner.unit.clone(), owner.module.clone()))
                        .collect(),
                },
                product: None,
            });
        }
        Ok((
            MaterializedExactDeclarationContext {
                artifacts,
                lexical: self.lexical.clone(),
            },
            references,
        ))
    }

    pub(crate) fn prepare_compilation(
        self: &Arc<Self>,
        root: &Path,
        producer: &[u8],
    ) -> Result<ExactCompilationRequest, CompileError> {
        self.prepare_compilation_with_authorization(root, producer, None)
    }

    /// The fixture adapter exits before its Haskell consumer runs. Its packet
    /// directory therefore owns the complete issued resource until consumption
    /// and cleanup, rather than borrowing the adapter process's temporary files.
    #[cfg(test)]
    pub(crate) fn prepare_fixture_compilation(
        self: &Arc<Self>,
        root: &Path,
        producer: &[u8],
    ) -> Result<ExactCompilationRequest, CompileError> {
        if !root.is_absolute()
            || self.inventory.retained_materialization().is_some()
            || self.producer
                != crate::artifact_inventory::CanonicalProducerIdentity::from_producer_bytes(
                    producer,
                )
                .sha256()
        {
            return Err(failure(
                "fixture delivery requires a fresh matched artifact owner",
            ));
        }
        std::fs::create_dir_all(root)?;
        let metadata = self.inventory.metadata_snapshot();
        metadata.validate_native_selection()?;
        self.inventory.retain_materialization(|parents| {
            if !parents.is_empty() {
                return Err(failure(
                    "fixture delivery cannot borrow process-local artifact owners",
                ));
            }
            let directory = tempfile::Builder::new()
                .prefix("tidepool-exact-artifacts-")
                .tempdir_in(root)?;
            let mut retained =
                self.materialize_retained_artifacts(&metadata, parents, directory)?;
            // These paths already belong to the packet's scoped directory. Its
            // owner releases them after the downstream process consumes them.
            retained._directory.disable_cleanup(true);
            Ok(retained)
        })?;
        self.prepare_compilation(root, producer)
    }

    pub(crate) fn prepare_compilation_with_authorization(
        self: &Arc<Self>,
        root: &Path,
        producer: &[u8],
        authorization: Option<Value>,
    ) -> Result<ExactCompilationRequest, CompileError> {
        let metadata = self.inventory.metadata_snapshot();
        metadata.validate_native_selection()?;
        let semantic_sha256 = self.semantic_sha256_from_metadata(&metadata);
        self.prepare_compilation_from_metadata(
            root,
            producer,
            authorization,
            &metadata,
            semantic_sha256,
        )
    }

    /// Bind authorization and the request to one observation of this immutable
    /// context. The caller cannot supply an unrelated semantic identity.
    pub(crate) fn prepare_compilation_authorizing(
        self: &Arc<Self>,
        root: &Path,
        producer: &[u8],
        authorize: impl FnOnce([u8; 32]) -> Result<Value, CompileError>,
    ) -> Result<ExactCompilationRequest, CompileError> {
        let metadata = self.inventory.metadata_snapshot();
        metadata.validate_native_selection()?;
        let semantic_sha256 = self.semantic_sha256_from_metadata(&metadata);
        let authorization = authorize(semantic_sha256)?;
        self.prepare_compilation_from_metadata(
            root,
            producer,
            Some(authorization),
            &metadata,
            semantic_sha256,
        )
    }

    fn materialize_retained_artifacts(
        &self,
        metadata: &ArtifactMetadataSnapshot,
        parents: Vec<Arc<RetainedArtifactMaterialization>>,
        directory: tempfile::TempDir,
    ) -> Result<RetainedArtifactMaterialization, CompileError> {
        let root = directory.path();
        let mut inherited_rows = BTreeMap::new();
        for parent in &parents {
            inherited_rows.extend(parent.selected_rows(metadata));
        }
        let inherited_row_count = inherited_rows.len();
        let entries = metadata.entries.values().cloned().collect::<Vec<_>>();
        let new_entries = entries
            .iter()
            .filter(|entry| !inherited_rows.contains_key(&entry.descriptor.id))
            .cloned()
            .collect::<Vec<_>>();
        let mut rows = BTreeMap::new();
        let mut validation = PackageInterfaceValidation::default();
        let context_bytes = materialization_bytes(&new_entries);
        let start = std::time::Instant::now();
        let (materialized, _) = self.materialize_entries_with_validation(
            root,
            &new_entries,
            &mut validation,
            MaterializationMode::Scratch,
        )?;
        for artifact in materialized.artifacts {
            let entry =
                &metadata.entries[&identity(&artifact.interface.unit, &artifact.interface.module)];
            let interface_evidence = scope_interface_evidence(entry, root, &mut validation)?;
            rows.insert(
                entry.descriptor.id,
                RetainedArtifactRow {
                    artifact,
                    interface_evidence,
                },
            );
        }
        crate::timing::record_stage(
            crate::timing::NO_NODE,
            crate::timing::NO_ROUND,
            "exact.context_materialize",
            start.elapsed(),
            context_bytes,
        );
        let mut selected_rows = inherited_rows;
        selected_rows.extend(rows.iter().map(|(id, row)| (*id, row)));
        let artifacts = entries
            .iter()
            .map(|entry| selected_rows[&entry.descriptor.id].artifact.clone())
            .collect::<Vec<_>>();
        let start = std::time::Instant::now();
        // The recovery writer issued every row from owned immutable bytes and
        // checked its private file once. Validate selection metadata without
        // rereading those same private payloads.
        self.validate_artifacts_with_owned_rows(&artifacts, metadata, &selected_rows)?;
        crate::timing::record_stage(
            crate::timing::NO_NODE,
            crate::timing::NO_ROUND,
            "exact.context_validate",
            start.elapsed(),
            context_bytes,
        );
        let available_entries = metadata.artifacts.values().cloned().collect::<Vec<_>>();
        let available = original_products_by_id(&available_entries);
        let inherited_groups = RetainedArtifactMaterialization::selected_group_refs(
            parents.iter().map(Arc::as_ref),
            metadata,
        )
        .into_iter()
        .cloned()
        .collect::<Vec<_>>();
        let inherited_group_handle_clones = inherited_groups.len();
        let start = std::time::Instant::now();
        let groups = certify_selected_owned_products_in_context_with_validation(
            &available,
            &inherited_groups,
            &metadata.selected_native_groups,
            &mut validation,
        )
        .map_err(failure)?;
        crate::timing::record_stage(
            crate::timing::NO_NODE,
            crate::timing::NO_ROUND,
            "exact.inherited_groups",
            start.elapsed(),
            context_bytes,
        );
        let mut graph_paths = parents
            .iter()
            .flat_map(|parent| parent.graph_path_refs())
            .map(|(digest, path)| (digest, path.clone()))
            .collect::<BTreeMap<_, _>>();
        let inherited_graphs = graph_paths.keys().copied().collect::<BTreeSet<_>>();
        let mut scope_written_bytes = 0;
        let execution_scope = execution_scope_value_with_graph_paths(
            &entries,
            root,
            &mut graph_paths,
            &mut scope_written_bytes,
        )?;
        let work = validation.work();
        tracing::info!(target: "tidepool_toolchain::artifacts", phase = "exact_immutable_materialization",
            retained_entries = inherited_row_count, new_entries = new_entries.len(),
            retained_metadata_row_clones = 0, inherited_row_lookups = inherited_row_count, inherited_group_handle_clones,
            transient_graph_path_clones = inherited_graphs.len(),
            retained_graph_paths = graph_paths.len() - inherited_graphs.len(),
            recovery_read_bytes = work.read_bytes, recovery_written_bytes = work.written_bytes,
            recovery_decoded_bytes = work.decoded_bytes, recovery_hash_bytes = work.hash_bytes,
            scope_written_bytes, "completed private artifact ownership");
        Ok(RetainedArtifactMaterialization {
            _directory: directory,
            _parents: parents,
            rows,
            groups: groups.into(),
            execution_scope,
            graph_paths: graph_paths
                .into_iter()
                .filter(|(digest, _)| !inherited_graphs.contains(digest))
                .collect(),
            #[cfg(test)]
            payload_work: work,
        })
    }

    fn prepare_compilation_from_metadata(
        self: &Arc<Self>,
        root: &Path,
        producer: &[u8],
        authorization: Option<Value>,
        metadata: &ArtifactMetadataSnapshot,
        semantic_sha256: [u8; 32],
    ) -> Result<ExactCompilationRequest, CompileError> {
        let admitted_empty = authorization.is_some()
            && self.producer == [0; 32]
            && metadata.entries.is_empty()
            && self.lexical.is_empty();
        if !root.is_absolute()
            || (!admitted_empty
                && (self.producer == [0; 32]
                    || self.producer != crate::artifact_inventory::CanonicalProducerIdentity::from_producer_bytes(producer).sha256()))
        {
            return Err(failure(
                "compile request has a different producer or invalid root",
            ));
        }
        std::fs::create_dir_all(root)?;
        let reused = self.inventory.retained_materialization().is_some();
        let retained = self.inventory.retain_materialization(|parents| {
            let directory = tempfile::Builder::new()
                .prefix("tidepool-exact-artifacts-")
                .tempdir()?;
            self.materialize_retained_artifacts(metadata, parents, directory)
        })?;
        let selected_rows = retained.selected_rows(metadata);
        let artifacts = metadata
            .entries
            .values()
            .map(|entry| selected_rows[&entry.descriptor.id].artifact.clone())
            .collect::<Vec<_>>();
        let groups: Arc<[PendingCertifiedGroup]> =
            RetainedArtifactMaterialization::selected_group_refs([retained.as_ref()], metadata)
                .into_iter()
                .cloned()
                .collect::<Vec<_>>()
                .into();
        let artifacts_by_owner = artifacts
            .iter()
            .map(|artifact| {
                (
                    (
                        artifact.interface.unit.as_str(),
                        artifact.interface.module.as_str(),
                    ),
                    artifact,
                )
            })
            .collect::<BTreeMap<_, _>>();
        let execution_scope = retained.execution_scope.clone();
        let fields = vec![
            text("TPEXACTSCOPE"),
            text("2"),
            text(hex(&semantic_sha256)),
            text(
                crate::artifact_inventory::CanonicalProducerIdentity::from_producer_bytes(producer)
                    .hex(),
            ),
            Value::Array(
                artifacts
                    .iter()
                    .map(|artifact| {
                        let iface = &artifact.interface;
                        let packages = iface.path.with_extension("hi.packages");
                        Ok(Value::Array(vec![
                            text(&iface.unit),
                            text(&iface.module),
                            path_value(&iface.path)?,
                            text(&iface.sha256),
                            Value::Array(
                                iface
                                    .requirements
                                    .iter()
                                    .map(|(unit, module)| {
                                        Value::Array(vec![text(unit), text(module)])
                                    })
                                    .collect(),
                            ),
                            path_value(&packages)?,
                            text(hex(&metadata.entries
                                [&identity(&iface.unit, &iface.module)]
                                .descriptor
                                .package_imports_sha256)),
                            selected_rows[&metadata.entries[&identity(&iface.unit, &iface.module)]
                                .descriptor
                                .id]
                                .interface_evidence
                                .clone(),
                        ]))
                    })
                    .collect::<Result<Vec<_>, CompileError>>()?,
            ),
            Value::Array(
                self.lexical
                    .iter()
                    .map(|node| {
                        Value::Array(vec![
                            module_value(&node.owner),
                            Value::Array(node.imports.iter().map(module_value).collect()),
                        ])
                    })
                    .collect(),
            ),
            Value::Array(
                metadata
                    .entries
                    .values()
                    .filter_map(|entry| match &entry.payload {
                        ArtifactPayload::Original(product) => Some(product),
                        _ => None,
                    })
                    .map(|product| {
                        let owner = product.owner();
                        let artifact = artifacts_by_owner
                            .get(&(owner.unit.as_str(), owner.module.as_str()))
                            .and_then(|artifact| artifact.product.as_ref())
                            .ok_or_else(|| failure("original product anchor is missing"))?;
                        Ok(Value::Array(vec![
                            text(&owner.unit),
                            text(&owner.module),
                            text(hex(&owner.module_version.0)),
                            text(hex(&owner.skinny_iface_sha256)),
                            text(hex(&owner.product_sha256)),
                            path_value(&artifact.path)?,
                            Value::Array(
                                groups
                                    .iter()
                                    .filter(|group| group.owner() == owner)
                                    .map(|group| {
                                        Value::Array(vec![
                                            Value::Integer(group.group().original_ordinal().into()),
                                            Value::Array(
                                                group
                                                    .group()
                                                    .binders()
                                                    .iter()
                                                    .map(symbol_value)
                                                    .collect(),
                                            ),
                                            Value::Array(
                                                group
                                                    .group()
                                                    .globals()
                                                    .iter()
                                                    .map(|global| {
                                                        Value::Array(vec![
                                                            symbol_value(&global.identity),
                                                            Value::Bool(
                                                                global
                                                                    .required_generation
                                                                    .is_none(),
                                                            ),
                                                        ])
                                                    })
                                                    .collect(),
                                            ),
                                        ])
                                    })
                                    .collect(),
                            ),
                        ]))
                    })
                    .collect::<Result<Vec<_>, CompileError>>()?,
            ),
        ];
        let bytes = encode_scope_manifest(fields, execution_scope, authorization)?;
        let manifest = root.join("exact-declaration-scope.cbor");
        use std::io::Write;
        let mut output = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&manifest)?;
        output.write_all(&bytes)?;
        tracing::info!(target: "tidepool_toolchain::artifacts", phase = "exact_request_references",
            reused_materialization = reused, manifest_written_bytes = bytes.len() as u64,
            selected_artifacts = artifacts.len(), "exact request references retained immutable products");
        Ok(ExactCompilationRequest {
            context: self.clone(),
            manifest,
            request_sha256: sha256(&bytes),
            semantic_sha256,
            producer_sha256:
                crate::artifact_inventory::CanonicalProducerIdentity::from_producer_bytes(producer)
                    .sha256(),
            artifacts,
            groups,
            materialization: Some(retained),
            program_support: None,
            program_source_lexical: Vec::new(),
            source_selected_support: BTreeSet::new(),
            source_search_include: None,
            checked_value_imports: Default::default(),
            generated_scaffold_imports: Vec::new(),
        })
    }
}

#[cfg(test)]
pub(crate) fn certified_product_artifact_view(
    producer: [u8; 32],
    products: &[CertifiedRecoveryProduct],
    interfaces: &[crate::certified_products::CertifiedModuleInterface],
    baseline: Option<&ExactDeclarationContext>,
) -> Result<ArtifactView, CompileError> {
    certified_product_artifact_view_with_validation(
        producer,
        products,
        interfaces,
        &[],
        baseline,
        crate::artifact_inventory::NativeArtifactDemand::AllGroups,
        &mut PackageInterfaceValidation::default(),
    )
}

// Product, target and inventory admission compare every witness against the
// same bounded capture; inventory construction must not create one filesystem
// observation per original module.
pub(crate) fn certified_product_artifact_view_with_validation(
    producer: [u8; 32],
    products: &[CertifiedRecoveryProduct],
    interfaces: &[crate::certified_products::CertifiedModuleInterface],
    values: &[CertifiedValueInterface],
    baseline: Option<&ExactDeclarationContext>,
    demand: crate::artifact_inventory::NativeArtifactDemand<'_>,
    validation: &mut PackageInterfaceValidation,
) -> Result<ArtifactView, CompileError> {
    let view = baseline.map_or_else(
        || ArtifactInventory::default().empty_view(),
        |context| context.artifact_view().clone(),
    );
    let mut entries = interfaces
        .iter()
        .cloned()
        .map(ArtifactEntry::canonical)
        .collect::<Vec<_>>();
    for value in values {
        if value.interface().toolchain_identity_sha256() != producer {
            return Err(failure("captured value has another compiler producer"));
        }
        entries.push(ArtifactEntry::interface(
            value.interface().clone(),
            JoinedInterfaceRole::ValueInterface,
            value.requirements().to_vec(),
        ));
    }
    for product in products {
        entries.push(ArtifactEntry::original_with_validation(
            producer,
            product.clone(),
            validation,
        )?);
    }
    view.inventory().admit_shared_with_demand(
        &view,
        entries.into_iter().map(Arc::new).collect(),
        demand,
    )
}

pub(crate) fn certified_artifact_view(
    producer: [u8; 32],
    products: &[CertifiedRecoveryProduct],
    interfaces: &[ExactInterfaceOwner],
    joined: &[CertifiedJoinedInterface],
    compiled: &ArtifactView,
    baseline: Option<&ExactDeclarationContext>,
) -> Result<ArtifactView, CompileError> {
    let view = baseline.map_or_else(
        || ArtifactInventory::default().empty_view(),
        |context| context.artifact_view().clone(),
    );
    let mut validation = PackageInterfaceValidation::default();
    let selected = products
        .iter()
        .map(|product| {
            ArtifactEntry::original_with_validation(producer, product.clone(), &mut validation)
                .map(|entry| entry.descriptor.id)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let view = view.merge(&compiled.select_roots(selected)?)?;
    let mut entries = Vec::new();
    for product in products {
        let owner = identity(&product.owner().unit, &product.owner().module);
        interfaces
            .iter()
            .find(|interface| interface.owner == owner)
            .ok_or_else(|| failure("original interface metadata missing"))?;
        entries.push(ArtifactEntry::original_with_validation(
            producer,
            product.clone(),
            &mut validation,
        )?);
    }
    for join in joined {
        let owner = identity(join.unit(), join.module());
        let requirements = interfaces
            .iter()
            .find(|interface| interface.owner == owner)
            .ok_or_else(|| failure("joined interface metadata missing"))?
            .requirements
            .clone();
        entries.push(ArtifactEntry::interface(
            join.clone(),
            JoinedInterfaceRole::LexicalJoin,
            requirements,
        ));
    }
    view.inventory().admit(&view, entries)
}

fn text(value: impl Into<String>) -> Value {
    Value::Text(value.into())
}
fn module_value(owner: &ExactModuleIdentity) -> Value {
    Value::Array(vec![text(&owner.unit), text(&owner.module)])
}
fn path_value(path: &Path) -> Result<Value, CompileError> {
    path.to_str()
        .map(text)
        .ok_or_else(|| failure("non-UTF-8 artifact path"))
}
fn symbol_value(symbol: &tidepool_repr::execution_schema::SymbolIdentity) -> Value {
    Value::Array(vec![
        text(&symbol.unit),
        text(&symbol.module),
        text(&symbol.namespace),
        text(&symbol.occurrence),
        symbol.record_parent.as_ref().map_or(Value::Null, text),
    ])
}

fn hex(bytes: &[u8; 32]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
fn sha256(bytes: &[u8]) -> String {
    use sha2::Digest;
    format!("{:x}", sha2::Sha256::digest(bytes))
}

/// Compose selections by exact owner and unordered import set. Duplicate
/// edges inside an individual input remain malformed, and differing sets
/// for one owner never acquire authority by union.
fn compose_lexical_nodes<'a>(
    nodes: impl IntoIterator<Item = &'a ExactLexicalNode>,
) -> Result<Vec<ExactLexicalNode>, CompileError> {
    let mut selected: BTreeMap<ExactModuleIdentity, Vec<ExactModuleIdentity>> = BTreeMap::new();
    for node in nodes {
        let mut imports = node.imports.clone();
        imports.sort();
        if imports.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(failure("duplicate selected lexical import"));
        }
        if let Some(previous) = selected.get(&node.owner) {
            if previous != &imports {
                return Err(failure(format!(
                    "conflicting selected lexical imports for {}:{}",
                    node.owner.unit, node.owner.module
                )));
            }
        } else {
            selected.insert(node.owner.clone(), imports);
        }
    }
    Ok(selected
        .into_iter()
        .map(|(owner, imports)| ExactLexicalNode { owner, imports })
        .collect())
}

#[cfg(test)]
pub(crate) use tests::assert_source_selected_receipt_pairing;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifact_inventory::ArtifactKind;

    include!("declaration_context/program_source_support_history.rs");

    #[test]
    fn lexical_composition_retains_shared_owners_idempotently() {
        let node = ExactLexicalNode {
            owner: identity("fixture", "Consumer"),
            imports: vec![identity("fixture", "Second"), identity("fixture", "First")],
        };
        let mut reordered = node.clone();
        reordered.imports.reverse();
        let merged = compose_lexical_nodes([&node, &reordered]).unwrap();
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].owner, node.owner);
        assert_eq!(merged[0].imports, reordered.imports);
        assert_eq!(
            compose_lexical_nodes(merged.iter().chain(merged.iter())).unwrap(),
            merged
        );
    }

    #[test]
    fn lexical_composition_refuses_conflicting_or_duplicate_edges() {
        let node = ExactLexicalNode {
            owner: identity("fixture", "Consumer"),
            imports: vec![identity("fixture", "First")],
        };
        let mut conflicting = node.clone();
        conflicting.imports.push(identity("fixture", "Second"));
        assert!(compose_lexical_nodes([&node, &conflicting]).is_err());
        assert!(compose_lexical_nodes([&conflicting, &node]).is_err());
        let mut duplicate = node.clone();
        duplicate.imports.push(duplicate.imports[0].clone());
        assert!(compose_lexical_nodes([&duplicate]).is_err());
    }

    #[test]
    fn pure_preview_refuses_explicit_heap_inputs_and_omits_unrelated_certified_tags() {
        let retained = crate::certified_products::PendingImportOwner::Retained {
            identity: tidepool_repr::execution_schema::SymbolIdentity {
                unit: "main".into(),
                module: "Tidepool.Session.Val.G7".into(),
                namespace: "value".into(),
                occurrence: "groupStore".into(),
                record_parent: None,
            },
            generation: 7,
        };
        let native = retained_generation_inputs(
            RetainedGenerationPolicy::PreserveCertifiedDemand,
            BTreeMap::new(),
            [&retained].into_iter(),
        )
        .unwrap();
        assert_eq!(native.len(), 1, "ordinary native demand is preserved");
        assert!(retained_generation_inputs(
            RetainedGenerationPolicy::PureActivationPreview,
            BTreeMap::new(),
            [&retained].into_iter(),
        )
        .unwrap()
        .is_empty());
        assert!(
            retained_generation_inputs(
                RetainedGenerationPolicy::PureActivationPreview,
                native,
                [&retained].into_iter(),
            )
            .is_err(),
            "an explicitly requested heap input cannot be silently removed"
        );
    }

    type PolicyKey = (u8, u8, u8, u8, Option<u8>);
    type PolicyRow = (PolicyKey, u64, u8);
    type PolicySemanticKey = (String, String, String, String, Option<String>);

    fn policy_key(key: PolicyKey) -> PolicySemanticKey {
        (
            format!("unit{}", key.0),
            format!("Module{}", key.1),
            if key.2 == 0 { "value" } else { "constructor" }.into(),
            format!("binding{}", key.3),
            key.4.map(|parent| format!("Record{parent}")),
        )
    }

    fn policy_symbol(key: PolicyKey) -> tidepool_extract_cmd::SymbolIdentity {
        let (unit, module, namespace, occurrence, record_parent) = policy_key(key);
        tidepool_extract_cmd::SymbolIdentity {
            unit,
            module,
            namespace,
            occurrence,
            record_parent,
        }
    }

    fn policy_import(row: PolicyRow) -> crate::certified_products::PendingImportOwner {
        use crate::certified_products::PendingImportOwner;
        let (unit, module, namespace, occurrence, record_parent) = policy_key(row.0);
        let binder = tidepool_repr::execution_schema::SymbolIdentity {
            unit: unit.clone(),
            module: module.clone(),
            namespace,
            occurrence,
            record_parent,
        };
        match row.2 {
            0 => PendingImportOwner::Retained {
                identity: binder,
                generation: row.1,
            },
            1 => PendingImportOwner::RetainedPackage {
                unit,
                module,
                binder,
                generation: row.1,
                interface_digest: [1; 32],
            },
            2 => PendingImportOwner::Source {
                owner: tidepool_repr::execution_schema::CachedHomeOwner {
                    unit,
                    module,
                    module_version: tidepool_repr::execution_schema::ModuleVersion([2; 32]),
                    skinny_iface_sha256: [3; 32],
                    product_sha256: [4; 32],
                },
                original_ordinal: 0,
                binder,
            },
            3 => PendingImportOwner::Package {
                unit,
                module,
                binder,
                interface_digest: [5; 32],
            },
            _ => unreachable!(),
        }
    }

    fn policy_property_config() -> proptest::test_runner::Config {
        let mut config = proptest::test_runner::Config::default();
        if let Some(path) = option_env!("TIDEPOOL_PROPTEST_REGRESSIONS") {
            config.failure_persistence = Some(Box::new(
                proptest::test_runner::FileFailurePersistence::Direct(path),
            ));
        }
        config
    }

    proptest::proptest! {
        #![proptest_config(policy_property_config())]
        #[test]
        fn retained_purpose_matrix_matches_list_oracle_under_unrelated_and_repeated_imports(
            tail in proptest::collection::vec(
                ((0u8..3, 0u8..3, 0u8..2, 0u8..3, proptest::option::of(0u8..3)), 1u64..4, 0u8..4),
                0..20,
            ),
            explicit in proptest::collection::vec(
                ((0u8..3, 0u8..3, 0u8..2, 0u8..3, proptest::option::of(0u8..3)), 1u64..4),
                0..8,
            ),
        ) {
            // Representation-only policy rows issue no certificate or site authority.
            // Every case includes both retained roles, ignored conflicting rows,
            // exact duplication, and each identity field distinguished independently.
            let first = (0, 0, 0, 0, None);
            let package = (1, 1, 0, 1, None);
            let mut rows = vec![
                (first, 1, 0), (package, 2, 1), (first, 3, 2),
                (package, 3, 3), (first, 1, 0), ((0, 0, 0, 0, Some(0)), 2, 0),
                ((1, 0, 0, 0, None), 2, 1),
                ((0, 1, 0, 0, None), 3, 0),
                ((0, 0, 1, 0, None), 2, 0),
                ((0, 0, 0, 1, None), 3, 1),
            ];
            rows.extend(tail);
            let explicit: BTreeMap<_, _> = explicit.into_iter()
                .map(|(key, generation)| (policy_symbol(key), generation)).collect();
            for request in [
                BTreeMap::new(), explicit,
                BTreeMap::from([(policy_symbol(first), 3)]),
                BTreeMap::from([(policy_symbol(package), 1)]),
            ] {
                // Independent oracle scans semantic pairs; it never calls the
                // production union routine or constructs admitted native evidence.
                let mut pairs = request.iter().map(|(key, generation)| (
                    (key.unit.clone(), key.module.clone(), key.namespace.clone(),
                        key.occurrence.clone(), key.record_parent.clone()), *generation,
                )).collect::<Vec<_>>();
                pairs.extend(rows.iter().filter(|row| row.2 < 2)
                    .map(|row| (policy_key(row.0), row.1)));
                let conflicts = pairs.iter().any(|(key, generation)| pairs.iter()
                    .any(|(other, value)| key == other && generation != value));
                pairs.sort();
                pairs.dedup();
                for order in [rows.clone(), rows.iter().copied().rev().collect(), rows.repeat(2)] {
                    let imports = order.into_iter().map(policy_import).collect::<Vec<_>>();
                    let native = retained_generation_inputs(
                        RetainedGenerationPolicy::PreserveCertifiedDemand,
                        request.clone(), imports.iter(),
                    );
                    proptest::prop_assert_eq!(native.is_err(), conflicts);
                    if let Ok(native) = native {
                        let mut actual = native.into_iter().map(|(key, generation)| (
                            (key.unit, key.module, key.namespace, key.occurrence, key.record_parent), generation,
                        )).collect::<Vec<_>>();
                        actual.sort();
                        proptest::prop_assert_eq!(&actual, &pairs);
                    }
                    let preview = retained_generation_inputs(
                        RetainedGenerationPolicy::PureActivationPreview,
                        request.clone(), imports.iter(),
                    );
                    proptest::prop_assert_eq!(preview.is_err(), !request.is_empty());
                    if let Ok(preview) = preview {
                        proptest::prop_assert!(preview.is_empty());
                    }
                }
            }
        }
    }

    #[test]
    fn certified_demand_tags_preserve_full_identity_and_refuse_generation_conflicts() {
        use crate::certified_products::PendingImportOwner;
        let identity = tidepool_repr::execution_schema::SymbolIdentity {
            unit: "main".into(),
            module: "Tidepool.Session.Val.G1".into(),
            namespace: "value".into(),
            occurrence: "x".into(),
            record_parent: None,
        };
        let retained = PendingImportOwner::Retained {
            identity: identity.clone(),
            generation: 1,
        };
        let mut package_identity = identity.clone();
        package_identity.unit = "package".into();
        package_identity.module = "PackageModule".into();
        let package = PendingImportOwner::RetainedPackage {
            unit: package_identity.unit.clone(),
            module: package_identity.module.clone(),
            binder: package_identity,
            generation: 3,
            interface_digest: [4; 32],
        };
        let tags =
            certified_retained_generation_tags(BTreeMap::new(), [&retained, &package].into_iter())
                .unwrap();
        assert_eq!(tags.len(), 2);
        assert!(certified_retained_generation_tags(tags.clone(), [&retained].into_iter()).is_ok());
        let conflict = PendingImportOwner::Retained {
            identity: identity.clone(),
            generation: 2,
        };
        assert!(certified_retained_generation_tags(tags, [&conflict].into_iter()).is_err());
        assert!(certified_retained_generation_tags(
            BTreeMap::new(),
            [&retained, &conflict].into_iter(),
        )
        .is_err());
        let mut distinct = identity;
        distinct.record_parent = Some("Record".into());
        let distinct = PendingImportOwner::Retained {
            identity: distinct,
            generation: 2,
        };
        assert_eq!(
            certified_retained_generation_tags(
                BTreeMap::new(),
                [&retained, &distinct].into_iter(),
            )
            .unwrap()
            .len(),
            2,
        );
    }
    use sha2::{Digest, Sha256};
    use tidepool_repr::execution_schema::{CachedHomeOwner, ModuleVersion};

    fn support_product(module: &str) -> CertifiedRecoveryProduct {
        support_product_in_unit("fixture", module)
    }

    fn support_product_in_unit(unit: &str, module: &str) -> CertifiedRecoveryProduct {
        support_product_with_interface(unit, module, format!("{module} interface").into_bytes())
    }

    fn support_product_with_interface(
        unit: &str,
        module: &str,
        interface: Vec<u8>,
    ) -> CertifiedRecoveryProduct {
        let mut product = Vec::new();
        ciborium::ser::into_writer(
            &Value::Array(vec![
                text("TPMOD"),
                Value::Integer(1.into()),
                Value::Array(vec![Value::Array(vec![
                    text(unit),
                    text(module),
                    Value::Bytes(interface.clone()),
                    Value::Array(vec![]),
                ])]),
            ]),
            &mut product,
        )
        .unwrap();
        let owner = CachedHomeOwner {
            unit: unit.into(),
            module: module.into(),
            module_version: ModuleVersion([1; 32]),
            skinny_iface_sha256: Sha256::digest(&interface).into(),
            product_sha256: Sha256::digest(&product).into(),
        };
        let certification =
            crate::certified_products::encode_home_certification(&owner, &[], &BTreeMap::new())
                .unwrap();
        let mut packages = Vec::new();
        ciborium::ser::into_writer(
            &Value::Array(vec![
                text("TPPKGROOTS"),
                text("2"),
                Value::Array(vec![
                    text(&owner.unit),
                    text(&owner.module),
                    text(sha256(&interface)),
                ]),
                Value::Array(vec![]),
                Value::Array(vec![]),
            ]),
            &mut packages,
        )
        .unwrap();
        crate::certified_products::fixture_finalized_product(
            CertifiedRecoveryProduct::from_certification(
                owner,
                interface,
                product,
                packages,
                certification,
            ),
            [2; 32],
        )
    }

    // Retain the old identity encoder to prove selection-aware keys invalidate it.
    fn legacy_semantic_sha256(context: &ExactDeclarationContext) -> [u8; 32] {
        use sha2::Digest;
        let mut lexical = context.lexical.iter().collect::<Vec<_>>();
        lexical.sort_by_key(|node| &node.owner);
        let value = Value::Array(vec![
            text("TPEXACTCONTEXT"),
            text("2"),
            text(hex(&sha2::Sha256::digest(
                serde_json::to_vec(&(
                    context.inventory.descriptors(),
                    context.inventory.dependencies(),
                ))
                .expect("inventory encoding"),
            )
            .into())),
            text(hex(&context.producer)),
            Value::Array(
                context
                    .recovery_products()
                    .iter()
                    .map(|product| {
                        let owner = product.owner();
                        Value::Array(vec![
                            text(&owner.unit),
                            text(&owner.module),
                            text(hex(&owner.module_version.0)),
                            text(hex(&owner.skinny_iface_sha256)),
                            text(hex(&owner.product_sha256)),
                            text(sha256(product.package_imports_bytes())),
                            text(sha256(product.certification_bytes())),
                        ])
                    })
                    .collect(),
            ),
            Value::Array(
                context
                    .joined_interfaces()
                    .iter()
                    .map(|join| {
                        Value::Array(vec![
                            text(join.unit()),
                            text(join.module()),
                            text(sha256(join.interface_bytes())),
                            text(sha256(join.package_imports_bytes())),
                        ])
                    })
                    .collect(),
            ),
            Value::Array(
                lexical
                    .into_iter()
                    .map(|node| {
                        let mut imports = node.imports.iter().collect::<Vec<_>>();
                        imports.sort();
                        Value::Array(vec![
                            module_value(&node.owner),
                            Value::Array(imports.into_iter().map(module_value).collect()),
                        ])
                    })
                    .collect(),
            ),
        ]);
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(&value, &mut bytes).expect("owned value encoding");
        sha2::Sha256::digest(bytes).into()
    }

    #[test]
    fn metadata_identity_invalidates_preselection_encoding_and_normalizes_lexical_order() {
        let (context, _) = metadata_fixture();
        assert_ne!(context.semantic_sha256(), legacy_semantic_sha256(&context));
        let mut reordered = context.as_ref().clone();
        reordered.lexical.reverse();
        assert_eq!(reordered.semantic_sha256(), context.semantic_sha256());
        assert_ne!(
            reordered.semantic_sha256(),
            legacy_semantic_sha256(&reordered)
        );
        let extended = context
            .as_ref()
            .clone()
            .extend_checked_original_products(
                context.producer,
                &[crate::certified_products::fixture_finalized_product(
                    support_product("Later"),
                    context.producer,
                )],
            )
            .unwrap();
        assert_ne!(extended.semantic_sha256(), context.semantic_sha256());
        assert_ne!(
            extended.semantic_sha256(),
            legacy_semantic_sha256(&extended)
        );
    }

    fn support_view(products: &[CertifiedRecoveryProduct]) -> ArtifactView {
        let producer = products
            .first()
            .and_then(|product| product.module_interface())
            .map_or([2; 32], |interface| interface.producer_sha256());
        certified_product_artifact_view(producer, products, &[], None).unwrap()
    }

    fn support_admission(root: &Path) -> ExactSourceAdmission {
        let modules = [
            ("InstanceOwner", None),
            ("InstanceRelay", Some("InstanceOwner")),
        ];
        let mut sources = Vec::new();
        let mut nodes = Vec::new();
        for (module, dependency) in modules {
            let path = root.join(format!("{module}.hs"));
            let source = format!("module {module} where\n");
            std::fs::write(&path, &source).unwrap();
            sources.push(crate::cache::SourceEvidence {
                path: path.clone(),
                sha256: sha256(source.as_bytes()),
            });
            nodes.push(crate::cache::ModuleEvidence {
                unit: "fixture".into(),
                module: module.into(),
                boot: false,
                source: path,
                imports: dependency
                    .into_iter()
                    .map(|dependency| crate::cache::ModuleImportEvidence {
                        qualifier: crate::cache::ImportQualifier::Unqualified,
                        module: dependency.into(),
                        boot: false,
                        selected: Some(root.join(format!("{dependency}.hs"))),
                    })
                    .collect(),
                product: crate::cache::ProductAvailability::Ready,
            });
        }
        let source = "module Target where\n";
        let target = root.join("Target.hs");
        std::fs::write(&target, source).unwrap();
        sources.push(crate::cache::SourceEvidence {
            path: target.clone(),
            sha256: sha256(source.as_bytes()),
        });
        let evidence = crate::cache::DependencyEvidence {
            version: 4,
            cache_safe: true,
            selection_complete: true,
            sources,
            resolutions: vec![crate::cache::ResolutionEvidence {
                qualifier: crate::cache::ImportQualifier::Unqualified,
                module: "InstanceOwner".into(),
                boot: false,
                selected: Some(root.join("InstanceOwner.hs")),
                candidates: vec![root.join("InstanceOwner.hs")],
            }],
            packages: vec![],
            modules: nodes,
        };
        let evidence_bytes = serde_json::to_vec(&evidence).unwrap();
        let evidence =
            crate::cache::CompletedSourceEvidence::from_worker_evidence(evidence, &target, source)
                .unwrap();
        ExactSourceAdmission {
            witness: ExactSourceWitness {
                source_path: target,
                source_sha256: Sha256::digest(source.as_bytes()).into(),
            },
            evidence_bytes,
            evidence,
            exact_imports: BTreeMap::new(),
            scaffold_roots: BTreeMap::new(),
            exact_source_imports: BTreeMap::new(),
            selected_originals: BTreeMap::new(),
        }
    }

    fn metadata_fixture() -> (Arc<ExactDeclarationContext>, Vec<u8>) {
        let producer = b"metadata producer".to_vec();
        let producer_sha256 = Sha256::digest(&producer).into();
        let interface = |module: &str| {
            let bytes = format!("{module} interface").into_bytes();
            let mut packages = Vec::new();
            ciborium::ser::into_writer(
                &Value::Array(vec![
                    text("TPPKGROOTS"),
                    text("2"),
                    Value::Array(vec![text("fixture"), text(module), text(sha256(&bytes))]),
                    Value::Array(Vec::new()),
                    Value::Array(vec![]),
                ]),
                &mut packages,
            )
            .unwrap();
            CertifiedJoinedInterface::from_certification(
                producer_sha256,
                "fixture".into(),
                module.into(),
                bytes,
                packages,
            )
            .unwrap()
        };
        let inventory = ArtifactInventory::default();
        let view = inventory
            .admit(
                &inventory.empty_view(),
                vec![
                    ArtifactEntry::original(
                        producer_sha256,
                        crate::certified_products::fixture_finalized_product(
                            support_product("Alpha"),
                            producer_sha256,
                        ),
                    )
                    .unwrap(),
                    ArtifactEntry::original(
                        producer_sha256,
                        crate::certified_products::fixture_finalized_product(
                            support_product("Beta"),
                            producer_sha256,
                        ),
                    )
                    .unwrap(),
                    ArtifactEntry::interface(
                        interface("Joined"),
                        JoinedInterfaceRole::LexicalJoin,
                        vec![identity("fixture", "Alpha"), identity("fixture", "Beta")],
                    ),
                    ArtifactEntry::interface(
                        interface("Value"),
                        JoinedInterfaceRole::ValueInterface,
                        vec![identity("fixture", "Alpha")],
                    ),
                ],
            )
            .unwrap();
        let context = ExactDeclarationContext {
            producer: producer_sha256,
            original_instance_environment: OriginalInstanceEnvironment::Unknown,
            template_imports: None,
            inventory: view,
            lexical: vec![
                ExactLexicalNode {
                    owner: identity("fixture", "Joined"),
                    imports: vec![identity("fixture", "Alpha")],
                },
                ExactLexicalNode {
                    owner: identity("fixture", "Alpha"),
                    imports: Vec::new(),
                },
            ],
        };
        context.normalize().unwrap();
        (Arc::new(context), producer)
    }

    #[test]
    fn recovery_capture_admits_embedded_canonical_and_refuses_corruption() {
        let producer = [2; 32];
        let root = tempfile::tempdir().unwrap();
        let package_path = root.path().join("External.hi");
        let package_bytes = [0x43];
        std::fs::write(&package_path, package_bytes).unwrap();
        let package_sha256: [u8; 32] = Sha256::digest(package_bytes).into();
        let mut binder = tidepool_repr::execution_schema::testing::identity("External", "entry");
        binder.unit = "external".into();
        let packages = BTreeMap::from([(
            ("external".into(), "External".into()),
            crate::certified_products::PackageInterfaceWitness {
                selected_path: package_path,
                sha256: package_sha256,
            },
        )]);
        let product = crate::certified_products::fixture_finalized_product(
            crate::certified_products::tests::original_witness_fixture(
                "Original",
                Some(crate::certified_products::PendingImportOwner::Package {
                    unit: "external".into(),
                    module: "External".into(),
                    binder,
                    interface_digest: package_sha256,
                }),
                7,
                &packages,
            ),
            producer,
        );
        let references = recovery_artifacts::materialize_certified_products(
            root.path(),
            producer,
            std::slice::from_ref(&product),
        )
        .unwrap();
        let embedded = references[0].module_interface.as_ref().unwrap();
        let captured =
            ExactDeclarationContext::capture_recovery(root.path(), &references, &[], &[], vec![])
                .unwrap();
        let recovered = captured.recovery_products();
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].owner(), product.owner());
        assert_eq!(recovered[0].product_bytes(), product.product_bytes());
        let duplicate_explicit = ExactDeclarationContext::capture_recovery(
            root.path(),
            &references,
            &[embedded.clone(), embedded.clone()],
            &[],
            vec![],
        )
        .unwrap();
        assert_eq!(
            captured.semantic_sha256(),
            duplicate_explicit.semantic_sha256()
        );
        let groups = crate::certified_products::certify_owned_products_with_validation(
            &[&recovered[0]],
            &[],
            &mut PackageInterfaceValidation::default(),
        )
        .unwrap();
        assert_eq!(groups.len(), 1);
        assert!(matches!(
            groups[0].imports()[0],
            crate::certified_products::PendingImportOwner::Package { ref unit, ref module, .. }
                if unit == "external" && module == "External"
        ));

        let mut missing = references.clone();
        missing[0].module_interface = None;
        assert!(
            ExactDeclarationContext::capture_recovery(root.path(), &missing, &[], &[], vec![])
                .is_err()
        );
        let mut changed = references.clone();
        changed[0]
            .module_interface
            .as_mut()
            .unwrap()
            .certificate_sha256 = [99; 32];
        assert!(
            ExactDeclarationContext::capture_recovery(root.path(), &changed, &[], &[], vec![])
                .is_err()
        );
        let mut conflicting_explicit = embedded.clone();
        conflicting_explicit.certificate_path = PathBuf::from("artifacts/missing-canonical.cbor");
        assert!(
            ExactDeclarationContext::capture_recovery(
                root.path(),
                &references,
                &[conflicting_explicit],
                &[],
                vec![],
            )
            .is_err(),
            "an invalid explicit descriptor cannot hide behind a matching embedded owner"
        );
        let certificate = root.path().join(&embedded.certificate_path);
        let mut corrupted = std::fs::read(&certificate).unwrap();
        corrupted[0] ^= 1;
        std::fs::write(certificate, corrupted).unwrap();
        assert!(ExactDeclarationContext::capture_recovery(
            root.path(),
            &references,
            &[],
            &[],
            vec![]
        )
        .is_err());
    }

    #[test]
    fn retained_projection_shared_lexical_rows_preserve_custody_and_reject_conflicts() {
        let (initial, _) = metadata_fixture();
        let shared = initial
            .lexical_graph()
            .iter()
            .find(|node| node.owner == identity("fixture", "Alpha"))
            .unwrap()
            .clone();
        let merged = initial
            .as_ref()
            .clone()
            .extend_lexical_joins(&[], &[shared.clone(), shared.clone()])
            .unwrap();
        assert_eq!(merged.semantic_sha256(), initial.semantic_sha256());
        assert_eq!(merged.artifact_view(), initial.artifact_view());
        assert_eq!(merged.recovery_products(), initial.recovery_products());
        assert_eq!(merged.lexical_graph().len(), initial.lexical_graph().len());
        assert!(!merged
            .lexical_graph()
            .iter()
            .any(|node| node.owner == identity("fixture", "Beta")));

        let mut conflicting = shared.clone();
        conflicting.imports.push(identity("fixture", "Joined"));
        let error = merged
            .extend_lexical_joins(&[], &[conflicting])
            .unwrap_err();
        assert!(matches!(error, CompileError::ExtractFailed(detail)
            if detail == "exact declaration context: conflicting selected lexical imports for fixture:Alpha"));

        // A constructor still requires one row per owner; only explicit
        // composition may combine equal selections from independent receipts.
        let mut malformed = initial.as_ref().clone();
        malformed.lexical.push(shared);
        let error = malformed.normalize().unwrap_err();
        assert!(matches!(error, CompileError::ExtractFailed(detail)
            if detail == "exact declaration context: duplicate selected lexical owner fixture:Alpha"));
    }

    #[test]
    fn ambiguous_native_owner_refuses_materialization_and_authorization() {
        let (initial, producer) = metadata_fixture();
        let existing = initial
            .inventory
            .entries_for_owners(std::iter::once(identity("fixture", "Alpha")))
            .unwrap();
        let ArtifactPayload::Original(product) = &existing[&identity("fixture", "Alpha")].payload
        else {
            panic!("native fixture")
        };
        let interface = product.module_interface().unwrap().clone();
        let mut owner = product.owner().clone();
        owner.module_version = tidepool_repr::execution_schema::ModuleVersion([9; 32]);
        let certification = crate::certified_products::encode_home_certification_with_module(
            &owner,
            &[],
            &BTreeMap::new(),
            interface.requirements(),
            <sha2::Sha256 as sha2::Digest>::digest(interface.certificate_bytes()).into(),
        )
        .unwrap();
        let variant = CertifiedRecoveryProduct::from_certification(
            owner,
            product.interface_bytes().to_vec(),
            product.product_bytes().to_vec(),
            product.package_imports_bytes().to_vec(),
            certification,
        )
        .with_module_interface(interface)
        .unwrap();
        let mut context = initial.as_ref().clone();
        context.inventory = context
            .inventory
            .inventory()
            .admit(
                &context.inventory,
                vec![ArtifactEntry::original(context.producer, variant).unwrap()],
            )
            .unwrap();
        let context = Arc::new(context);
        assert_ne!(initial.semantic_sha256(), context.semantic_sha256());
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("uncreated");
        assert!(
            matches!(context.materialize(&root), Err(CompileError::ArtifactInventory(error))
            if matches!(error.failure, ArtifactInventoryFailure::NativeOwnerAmbiguity { .. }))
        );
        assert!(!root.exists());
        assert!(
            matches!(context.prepare_compilation_authorizing(&root, &producer, |_| {
            panic!("ambiguous context must refuse before purpose authorization")
        }), Err(CompileError::ArtifactInventory(error))
            if matches!(error.failure, ArtifactInventoryFailure::NativeOwnerAmbiguity { .. }))
        );
        assert!(!root.exists());
    }

    #[test]
    fn exact_scope_evidence_uses_admitted_artifact_roles() {
        let (context, _) = metadata_fixture();
        let root = tempfile::tempdir().unwrap();
        let metadata = context.inventory.metadata_snapshot();
        for entry in metadata.entries.values() {
            let value = scope_interface_evidence(
                entry,
                root.path(),
                &mut PackageInterfaceValidation::default(),
            )
            .unwrap();
            let row = value.as_array().unwrap();
            match entry.descriptor.kind {
                ArtifactKind::OriginalModule | ArtifactKind::CanonicalModuleInterface => {
                    assert_eq!(row.len(), 5);
                    assert_eq!(row[0], text("module"));
                    assert!(row[3].as_text().is_some());
                    assert!(row[4].as_text().is_some());
                }
                ArtifactKind::LexicalJoin => assert_eq!(row, &[text("join")]),
                ArtifactKind::ValueInterface => assert_eq!(row, &[text("value")]),
            }
        }
        let mut forged = metadata.entries[&identity("fixture", "Joined")]
            .as_ref()
            .clone();
        forged.descriptor.kind = ArtifactKind::CanonicalModuleInterface;
        assert!(scope_interface_evidence(
            &forged,
            root.path(),
            &mut PackageInterfaceValidation::default()
        )
        .is_err());
    }

    #[test]
    fn source_original_scope_role_is_not_inferred_from_native_module_spelling() {
        let interface = crate::certified_products::fixture_module_interface(
            [7; 32],
            "main",
            "Tidepool.Session.Lib.G1",
            BTreeMap::new(),
        );
        let entry = ArtifactEntry::canonical(interface);
        let root = tempfile::tempdir().unwrap();
        let value = scope_interface_evidence(
            &entry,
            root.path(),
            &mut PackageInterfaceValidation::default(),
        )
        .unwrap();
        assert_eq!(value.as_array().unwrap()[0], text("module"));
        let mut conflicting = entry;
        conflicting.descriptor.owner.unit = "other".into();
        assert!(scope_interface_evidence(
            &conflicting,
            root.path(),
            &mut PackageInterfaceValidation::default()
        )
        .is_err());
    }

    #[test]
    fn metadata_validation_indexes_owners_and_rechecks_late_corruption() {
        let (context, _) = metadata_fixture();
        let directory = tempfile::tempdir().unwrap();
        let artifacts = context.materialize_scratch(&directory).unwrap().artifacts;
        let before = context.inventory.inventory().metrics();
        context.validate_artifacts(&artifacts).unwrap();
        let after = context.inventory.inventory().metrics();
        assert_eq!(after.graph_visits - before.graph_visits, 6);
        assert_eq!(after.view_queries - before.view_queries, 1);
        assert_eq!(after.entry_handle_copies - before.entry_handle_copies, 10);
        assert!(context.validate_artifacts(&artifacts[..3]).is_err());
        let mut duplicate = artifacts.clone();
        duplicate.push(artifacts[0].clone());
        assert!(context.validate_artifacts(&duplicate).is_err());
        let mut wrong_kind = artifacts.clone();
        wrong_kind[0].product = None;
        assert!(context.validate_artifacts(&wrong_kind).is_err());
        let mut wrong_kind = artifacts.clone();
        wrong_kind[2].product = artifacts[0].product.clone();
        assert!(context.validate_artifacts(&wrong_kind).is_err());
        let mut wrong_requirements = artifacts.clone();
        let required = wrong_requirements
            .iter_mut()
            .find(|artifact| artifact.interface.module == "Joined")
            .unwrap();
        assert!(!required.interface.requirements.is_empty());
        required.interface.requirements.clear();
        assert!(context.validate_artifacts(&wrong_requirements).is_err());
        let mut foreign = artifacts.clone();
        foreign[0].interface.module = "Foreign".into();
        assert!(context.validate_artifacts(&foreign).is_err());
        let mut entries = context
            .inventory
            .entries()
            .iter()
            .map(|entry| entry.as_ref().clone())
            .collect::<Vec<_>>();
        let entry = entries
            .iter_mut()
            .find(|entry| entry.descriptor.owner.module == "Joined")
            .unwrap();
        // Corrupted descriptor metadata cannot turn an interface into native evidence.
        entry.descriptor.kind = ArtifactKind::OriginalModule;
        let inventory = ArtifactInventory::default();
        let mut wrong_kind = context.as_ref().clone();
        wrong_kind.inventory = inventory.admit(&inventory.empty_view(), entries).unwrap();
        assert!(wrong_kind.validate_artifacts(&artifacts).is_err());
        let paths = [
            artifacts[0].interface.path.clone(),
            artifacts[0].product.as_ref().unwrap().path.clone(),
            artifacts[0].interface.path.with_extension("hi.packages"),
        ];
        for path in paths {
            let saved = std::fs::read(&path).unwrap();
            std::fs::write(&path, b"changed after preparation").unwrap();
            assert!(context.validate_artifacts(&artifacts).is_err());
            std::fs::write(&path, saved).unwrap();
            context.validate_artifacts(&artifacts).unwrap();
        }
    }

    #[test]
    fn metadata_authorization_and_preparation_share_one_identity() {
        let (context, producer) = metadata_fixture();
        let directory = tempfile::tempdir().unwrap();
        let expected = context.semantic_sha256();
        let before = context.inventory.inventory().metrics();
        let request = context
            .prepare_compilation_authorizing(directory.path(), &producer, |semantic_sha256| {
                assert_eq!(semantic_sha256, expected);
                Ok(Value::Array(vec![
                    text("authorized"),
                    text(hex(&semantic_sha256)),
                ]))
            })
            .unwrap();
        let after = context.inventory.inventory().metrics();
        assert_eq!(after.graph_visits - before.graph_visits, 6);
        assert_eq!(after.view_queries - before.view_queries, 1);
        assert_eq!(request.semantic_sha256, expected);
        let value: Value =
            ciborium::de::from_reader(std::fs::read(&request.manifest).unwrap().as_slice())
                .unwrap();
        let fields = row(&value, 9).unwrap();
        assert_eq!(string(&fields[2]).unwrap(), hex(&expected));
        assert_eq!(row(&fields[8], 2).unwrap()[1], text(hex(&expected)));
        let mut foreign = request.artifacts.clone();
        foreign[0].interface.sha256 = sha256(b"different claimed owner");
        assert!(request.context.validate_artifacts(&foreign).is_err());
    }

    #[test]
    fn fixture_delivery_transfers_materialization_cleanup_to_packet_owner() {
        let (context, producer) = metadata_fixture();
        let packet = tempfile::tempdir().unwrap();
        let request = context
            .prepare_fixture_compilation(packet.path(), &producer)
            .unwrap();
        let paths = request
            .artifacts
            .iter()
            .map(|artifact| artifact.interface.path.clone())
            .collect::<Vec<_>>();
        let owned_root = request
            .materialization
            .as_ref()
            .unwrap()
            ._directory
            .path()
            .to_path_buf();
        assert!(owned_root.starts_with(packet.path()));
        assert!(!paths.is_empty());
        assert!(paths.iter().all(|path| path.starts_with(&owned_root)));
        assert!(context
            .prepare_fixture_compilation(&packet.path().join("second"), &producer)
            .is_err());
        assert!(!packet.path().join("second").exists());
        let weak = Arc::downgrade(request.materialization.as_ref().unwrap());
        drop(request);
        drop(context);
        assert!(weak.upgrade().is_none());
        assert!(paths.iter().all(|path| path.is_file()));
        drop(packet);
        assert!(!owned_root.exists());
        assert!(paths.iter().all(|path| !path.exists()));
    }

    #[test]
    fn retained_materialization_survives_request_scratch_and_releases_with_owner() {
        let (context, producer) = metadata_fixture();
        let scratch = tempfile::tempdir().unwrap();
        let first = context
            .prepare_compilation(&scratch.path().join("first"), &producer)
            .unwrap();
        let paths = first.artifacts.clone();
        let groups = Arc::clone(&first.groups);
        let retained = first.materialization.as_ref().unwrap();
        let owned_root = retained._directory.path().to_path_buf();
        let weak = Arc::downgrade(retained);
        let semantic = first.semantic_sha256;
        drop(first);
        std::fs::remove_dir_all(scratch.path().join("first")).unwrap();
        assert!(paths
            .iter()
            .all(|artifact| artifact.interface.path.is_file()));
        let second = context
            .prepare_compilation(&scratch.path().join("second"), &producer)
            .unwrap();
        assert_eq!(second.artifacts, paths);
        assert_eq!(second.semantic_sha256, semantic);
        assert_eq!(groups, second.groups);
        assert!(groups
            .iter()
            .zip(second.groups.iter())
            .all(|(left, right)| std::ptr::eq(left.group(), right.group())));
        assert!(Arc::ptr_eq(
            &weak.upgrade().unwrap(),
            second.materialization.as_ref().unwrap()
        ));
        let manifest: Value =
            ciborium::de::from_reader(std::fs::read(&second.manifest).unwrap().as_slice()).unwrap();
        for (artifact, row) in paths
            .iter()
            .zip(manifest.as_array().unwrap()[4].as_array().unwrap())
        {
            assert_eq!(
                row.as_array().unwrap()[2],
                path_value(&artifact.interface.path).unwrap()
            );
        }
        drop(context);
        assert!(
            owned_root.is_dir(),
            "the current request retains the private owner"
        );
        drop(second);
        assert!(weak.upgrade().is_none());
        assert!(
            !owned_root.exists(),
            "last owner release removes private materialization"
        );
    }

    #[test]
    fn retained_materialization_prefix_a_b_a_borrows_only_unchanged_entries() {
        let (a, producer) = metadata_fixture();
        let scratch = tempfile::tempdir().unwrap();
        let first = a
            .prepare_compilation(&scratch.path().join("a1"), &producer)
            .unwrap();
        let gamma = crate::certified_products::fixture_finalized_product(
            support_product("Gamma"),
            a.producer,
        );
        let b = Arc::new(
            a.as_ref()
                .clone()
                .extend_checked_original_products(a.producer, &[gamma])
                .unwrap(),
        );
        let second = b
            .prepare_compilation(&scratch.path().join("b"), &producer)
            .unwrap();
        for artifact in &first.artifacts {
            assert!(
                second.artifacts.contains(artifact),
                "B references A's unchanged private paths"
            );
        }
        assert_eq!(second.artifacts.len(), first.artifacts.len() + 1);
        let b_owner = second.materialization.as_ref().unwrap();
        assert_eq!(b_owner._parents.len(), 1);
        assert!(Arc::ptr_eq(
            &b_owner._parents[0],
            first.materialization.as_ref().unwrap()
        ));
        let gamma = second
            .artifacts
            .iter()
            .find(|artifact| artifact.interface.module == "Gamma")
            .unwrap();
        assert!(gamma.interface.path.starts_with(b_owner._directory.path()));
        let third = a
            .prepare_compilation(&scratch.path().join("a2"), &producer)
            .unwrap();
        assert_eq!(third.artifacts, first.artifacts);
        assert_eq!(third.semantic_sha256, first.semantic_sha256);
        assert_ne!(third.semantic_sha256, second.semantic_sha256);
        assert!(Arc::ptr_eq(
            third.materialization.as_ref().unwrap(),
            first.materialization.as_ref().unwrap()
        ));
        let root_a = first
            .materialization
            .as_ref()
            .unwrap()
            ._directory
            .path()
            .to_path_buf();
        drop(first);
        drop(third);
        drop(a);
        assert!(root_a.exists(), "B retains borrowed A paths");
        drop(second);
        drop(b);
        assert!(!root_a.exists());
    }

    #[test]
    fn retained_materialization_nonempty_groups_select_versions_and_shared_parents() {
        let (initial, producer) = metadata_fixture();
        let product = |module, version, generation| {
            let original = crate::certified_products::fixture_finalized_product(
                crate::certified_products::tests::original_witness_fixture(
                    module,
                    Some(crate::certified_products::PendingImportOwner::Retained {
                        identity: tidepool_repr::execution_schema::testing::identity(
                            "Value", "live",
                        ),
                        generation,
                    }),
                    version,
                    &BTreeMap::new(),
                ),
                initial.producer,
            );
            crate::certified_products::tests::recovered_witness_fixtures(&[original])
                .remove(0)
                .product
        };
        let extend = |context: &Arc<ExactDeclarationContext>, product| {
            Arc::new(
                context
                    .as_ref()
                    .clone()
                    .extend_checked_original_products(context.producer, &[product])
                    .unwrap(),
            )
        };
        let scratch = tempfile::tempdir().unwrap();
        let base = extend(&initial, product("Native", 7, 11));
        let base_request = base
            .prepare_compilation(&scratch.path().join("base"), &producer)
            .unwrap();
        assert_eq!(base_request.groups.len(), 1);
        let left = extend(&base, product("Left", 8, 12));
        let right = extend(&base, product("Right", 9, 13));
        let left_request = left
            .prepare_compilation(&scratch.path().join("left"), &producer)
            .unwrap();
        let right_request = right
            .prepare_compilation(&scratch.path().join("right"), &producer)
            .unwrap();
        let mut joined = left.as_ref().clone();
        joined.inventory = joined.inventory.merge(&right.inventory).unwrap();
        joined.normalize().unwrap();
        let joined = Arc::new(joined);
        let joined_request = joined
            .prepare_compilation(&scratch.path().join("joined"), &producer)
            .unwrap();
        let joined_owner = joined_request.materialization.as_ref().unwrap();
        assert!(
            joined_owner.groups.is_empty(),
            "joining adds no native payload"
        );
        assert_eq!(joined_owner._parents.len(), 2);
        let base_owner = base_request.materialization.as_ref().unwrap();
        for branch in [&left_request, &right_request] {
            let owner = branch.materialization.as_ref().unwrap();
            assert_eq!(owner.groups.len(), 1);
            assert!(Arc::ptr_eq(&owner._parents[0], base_owner));
        }
        let mut visited = Vec::new();
        joined_owner.visit_owners(&mut BTreeSet::new(), &mut |owner| {
            visited.push(owner as *const RetainedArtifactMaterialization);
        });
        assert_eq!(visited.len(), 4, "the diamond visits its shared base once");
        assert_eq!(visited[0], Arc::as_ptr(base_owner));
        assert_eq!(visited[3], Arc::as_ptr(joined_owner));
        assert_eq!(
            joined_request
                .groups
                .iter()
                .map(|group| group.owner().module.as_str())
                .collect::<Vec<_>>(),
            ["Native", "Left", "Right"]
        );
        for branch in [&base_request, &left_request, &right_request] {
            let original = branch
                .materialization
                .as_ref()
                .unwrap()
                .groups
                .first()
                .unwrap();
            let selected = joined_request
                .groups
                .iter()
                .find(|group| group.owner() == original.owner())
                .unwrap();
            assert!(std::ptr::eq(selected.group(), original.group()));
            assert_eq!(selected.imports(), original.imports());
        }

        let replacement = extend(&initial, product("Native", 10, 22));
        let replacement_request = replacement
            .prepare_compilation(&scratch.path().join("replacement"), &producer)
            .unwrap();
        let replacement_owner = replacement_request.materialization.as_ref().unwrap();
        let selected = RetainedArtifactMaterialization::selected_group_refs(
            [joined_owner.as_ref(), replacement_owner.as_ref()],
            &replacement.inventory.metadata_snapshot(),
        );
        assert_eq!(
            selected.len(),
            1,
            "unselected modules and old versions are excluded"
        );
        assert_eq!(selected[0].owner().module_version.0, [10; 32]);
        assert!(std::ptr::eq(
            selected[0].group(),
            replacement_request.groups[0].group()
        ));
        assert!(matches!(
            selected[0].imports()[0],
            crate::certified_products::PendingImportOwner::Retained { generation: 22, .. }
        ));
    }

    #[test]
    fn retained_value_keeps_nonlexical_originals_and_projected_file_custody() {
        let (initial, producer) = metadata_fixture();
        let native = crate::certified_products::fixture_finalized_product(
            crate::certified_products::tests::original_witness_fixture(
                "HiddenNative",
                Some(crate::certified_products::PendingImportOwner::Retained {
                    identity: tidepool_repr::execution_schema::testing::identity("Value", "live"),
                    generation: 11,
                }),
                7,
                &BTreeMap::new(),
            ),
            initial.producer,
        );
        let native = crate::certified_products::tests::recovered_witness_fixtures(&[native])
            .remove(0)
            .product;
        let context = Arc::new(
            initial
                .as_ref()
                .clone()
                .extend_checked_original_products(initial.producer, &[native])
                .unwrap(),
        );
        let scratch = tempfile::tempdir().unwrap();
        let original = context
            .prepare_compilation(&scratch.path().join("original"), &producer)
            .unwrap();
        let private_owner = original.materialization.as_ref().unwrap();
        let private_root = private_owner._directory.path().to_path_buf();
        let weak = Arc::downgrade(private_owner);
        let value_entry = context
            .artifact_view()
            .entries_for_owners(std::iter::once(identity("fixture", "Value")))
            .unwrap();
        let value = context
            .artifact_view()
            .select_roots(vec![
                value_entry[&identity("fixture", "Value")].descriptor.id,
            ])
            .unwrap();
        let (retained, lexical) = context.retain_value_source_surface(&value, &[]).unwrap();
        assert!(lexical
            .iter()
            .all(|node| node.owner.module != "HiddenNative"));
        assert!(retained
            .descriptors()
            .iter()
            .any(|descriptor| descriptor.kind
                == crate::artifact_inventory::ArtifactKind::OriginalModule
                && descriptor.owner.module == "HiddenNative"));
        let interfaces = retained
            .interface_projection(
                &retained
                    .interface_owners()
                    .into_iter()
                    .map(|owner| owner.owner)
                    .collect::<Vec<_>>(),
            )
            .unwrap();
        assert!(interfaces
            .descriptors()
            .iter()
            .all(|descriptor| descriptor.kind
                != crate::artifact_inventory::ArtifactKind::OriginalModule));
        let mut next = context.as_ref().clone();
        next.inventory = retained;
        next.lexical = lexical;
        next.normalize().unwrap();
        let next = Arc::new(next);
        let request = next
            .prepare_compilation(&scratch.path().join("projected"), &producer)
            .unwrap();
        let projected_owner = request.materialization.as_ref().unwrap();
        assert!(projected_owner.rows.is_empty());
        assert_eq!(projected_owner.payload_work.written_bytes, 0);
        assert_eq!(request.groups.len(), 1);
        assert!(std::ptr::eq(
            request.groups[0].group(),
            original.groups[0].group()
        ));
        for artifact in &request.artifacts {
            assert!(original.artifacts.contains(artifact));
        }
        let blocked = program_request(scratch.path(), Arc::clone(&next));
        assert!(
            blocked
                .validate_receipt(
                    &import_receipt(scratch.path(), &blocked, "HiddenNative"),
                    None,
                    &next,
                )
                .is_err(),
            "retained executable availability grants no authored lexical import"
        );
        drop(original);
        drop(context);
        drop(value);
        drop(value_entry);
        drop(initial);
        assert!(
            private_root.is_dir(),
            "projected custody retains original private files"
        );
        drop(interfaces);
        drop(blocked);
        drop(request);
        drop(next);
        assert!(weak.upgrade().is_none());
        assert!(!private_root.exists());
    }

    #[test]
    fn retained_materialization_many_descendants_measure_metadata_and_payload_work() {
        for descendants in [24, 100] {
            let (mut context, producer) = metadata_fixture();
            let scratch = tempfile::tempdir().unwrap();
            let first = context
                .prepare_compilation(&scratch.path().join("base"), &producer)
                .unwrap();
            let mut previous = first.artifacts;
            let mut written_bytes = first
                .materialization
                .as_ref()
                .unwrap()
                .payload_work
                .written_bytes;
            let mut inherited_rows = 0;
            for index in 0..descendants {
                let product = crate::certified_products::fixture_finalized_product(
                    support_product(&format!("Growing{index}")),
                    context.producer,
                );
                context = Arc::new(
                    context
                        .as_ref()
                        .clone()
                        .extend_checked_original_products(context.producer, &[product])
                        .unwrap(),
                );
                let request = context
                    .prepare_compilation(&scratch.path().join(format!("child{index}")), &producer)
                    .unwrap();
                for artifact in &previous {
                    assert!(request.artifacts.contains(artifact));
                }
                let retained = request.materialization.as_ref().unwrap();
                assert_eq!(retained._parents.len(), 1);
                assert_eq!(
                    retained.rows.len(),
                    1,
                    "a descendant retains only its new row"
                );
                let mut stored_rows = 0;
                retained.visit_owners(&mut BTreeSet::new(), &mut |owner| {
                    stored_rows += owner.rows.len();
                });
                assert_eq!(
                    stored_rows,
                    index + 5,
                    "ancestor row storage grows linearly"
                );
                let retained_files =
                    std::fs::read_dir(retained._directory.path().join("artifacts"))
                        .unwrap()
                        .map(|entry| entry.unwrap().metadata().unwrap().len())
                        .sum::<u64>();
                assert_eq!(
                    retained.payload_work.written_bytes, retained_files,
                    "only this descendant's new private payloads were written"
                );
                assert!(retained.payload_work.written_bytes > 0);
                inherited_rows += previous.len();
                written_bytes += retained.payload_work.written_bytes;
                previous = request.artifacts;
            }
            assert_eq!(
                inherited_rows,
                descendants * 4 + descendants * (descendants - 1) / 2
            );
            let retained = context.inventory.retained_materialization().unwrap();
            let mut stored_rows = 0;
            let mut stored_groups = 0;
            let mut stored_graph_paths = 0;
            retained.visit_owners(&mut BTreeSet::new(), &mut |owner| {
                stored_rows += owner.rows.len();
                stored_groups += owner.groups.len();
                stored_graph_paths += owner.graph_paths.len();
            });
            assert_eq!(stored_rows, descendants + 4);
            println!("retained-descendants count={descendants} immutable_written_bytes={written_bytes} transient_inherited_rows={inherited_rows} retained_row_handles={stored_rows} retained_group_handles={stored_groups} retained_graph_handles={stored_graph_paths} final_rows={}", previous.len());
        }
    }

    #[test]
    fn metadata_identity_is_path_independent_and_tracks_graph_selection() {
        let (context, _) = metadata_fixture();
        let expected = context.semantic_sha256();
        let before = context.inventory.inventory().metrics();
        assert_eq!(context.semantic_sha256(), expected);
        let after = context.inventory.inventory().metrics();
        assert_eq!(after.graph_visits - before.graph_visits, 6);
        assert_eq!(after.view_queries - before.view_queries, 1);
        for _ in 0..2 {
            let directory = tempfile::tempdir().unwrap();
            context.materialize_scratch(&directory).unwrap();
            assert_eq!(context.semantic_sha256(), expected);
        }
        let mut changed = context.as_ref().clone();
        changed.producer = [9; 32];
        assert_ne!(changed.semantic_sha256(), expected);
        let mut changed = context.as_ref().clone();
        changed.lexical.clear();
        assert_ne!(changed.semantic_sha256(), expected);
        let mut reordered = context.as_ref().clone();
        reordered.lexical.reverse();
        assert_eq!(reordered.semantic_sha256(), expected);
        let extended = context
            .as_ref()
            .clone()
            .extend_checked_original_products(
                context.producer,
                &[crate::certified_products::fixture_finalized_product(
                    support_product("Later"),
                    context.producer,
                )],
            )
            .unwrap();
        assert_ne!(extended.semantic_sha256(), expected);
        let owners = context
            .interface_owners()
            .into_iter()
            .map(|interface| interface.owner)
            .collect::<Vec<_>>();
        let mut type_only = context.as_ref().clone();
        type_only.inventory = context.inventory.interface_projection(&owners).unwrap();
        type_only.normalize().unwrap();
        assert_eq!(type_only.interface_owners(), context.interface_owners());
        assert_eq!(type_only.lexical, context.lexical);
        assert!(type_only.recovery_products().is_empty());
        assert!(!context.recovery_products().is_empty());
        assert_ne!(type_only.semantic_sha256(), expected);
    }

    fn program_request(
        root: &Path,
        context: Arc<ExactDeclarationContext>,
    ) -> ExactCompilationRequest {
        let manifest = root.join("scope");
        std::fs::write(&manifest, b"scope").unwrap();
        ExactCompilationRequest {
            context,
            manifest,
            request_sha256: sha256(b"scope"),
            semantic_sha256: [1; 32],
            producer_sha256: [2; 32],
            artifacts: vec![],
            groups: Arc::from([]),
            materialization: None,
            program_support: None,
            program_source_lexical: Vec::new(),
            source_selected_support: BTreeSet::new(),
            source_search_include: None,
            checked_value_imports: Default::default(),
            generated_scaffold_imports: Vec::new(),
        }
    }

    fn import_receipt(root: &Path, request: &ExactCompilationRequest, imported: &str) -> PathBuf {
        import_receipt_owner(root, request, "fixture", imported, "none", false)
    }

    fn import_receipt_owner(
        root: &Path,
        request: &ExactCompilationRequest,
        unit: &str,
        imported: &str,
        qualifier: &str,
        boot: bool,
    ) -> PathBuf {
        import_receipt_source_owner(
            root,
            request,
            unit,
            imported,
            qualifier,
            boot,
            "module Consumer where\n",
        )
    }

    fn import_receipt_source_owner(
        root: &Path,
        request: &ExactCompilationRequest,
        unit: &str,
        imported: &str,
        qualifier: &str,
        boot: bool,
        source: &str,
    ) -> PathBuf {
        let directory = root.join(format!("receipt-{unit}-{imported}-{qualifier}-{boot}"));
        std::fs::create_dir_all(&directory).unwrap();
        let path = root.join("Consumer.hs");
        std::fs::write(&path, source).unwrap();
        let snapshot = directory.join("source.hs");
        std::fs::write(&snapshot, source).unwrap();
        let evidence = crate::cache::DependencyEvidence {
            version: 4,
            cache_safe: true,
            selection_complete: true,
            sources: vec![crate::cache::SourceEvidence {
                path: path.clone(),
                sha256: sha256(source.as_bytes()),
            }],
            resolutions: vec![],
            packages: vec![],
            modules: vec![crate::cache::ModuleEvidence {
                unit: "fixture".into(),
                module: "Consumer".into(),
                boot: false,
                source: path.clone(),
                imports: vec![],
                product: crate::cache::ProductAvailability::Ready,
            }],
        };
        let value = Value::Array(vec![
            text("TPEXACTCOMPILE"),
            text("3"),
            text(&request.request_sha256),
            text(hex(&request.semantic_sha256)),
            path_value(&path).unwrap(),
            text(sha256(source.as_bytes())),
            path_value(&snapshot).unwrap(),
            text(serde_json::to_string(&evidence).unwrap()),
            Value::Array(vec![Value::Array(vec![
                text("fixture"),
                text("Consumer"),
                Value::Bool(false),
                Value::Array(vec![Value::Array(vec![
                    text(qualifier),
                    text(imported),
                    Value::Bool(boot),
                    text(unit),
                ])]),
            ])]),
            Value::Array(vec![Value::Array(vec![]), Value::Null]),
        ]);
        let receipt = directory.join("receipt.cbor");
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(&value, &mut bytes).unwrap();
        std::fs::write(&receipt, bytes).unwrap();
        receipt
    }

    fn write_receipt(path: &Path, value: &Value) {
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(value, &mut bytes).unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    fn read_receipt(path: &Path) -> Value {
        ciborium::de::from_reader(std::fs::read(path).unwrap().as_slice()).unwrap()
    }

    #[test]
    fn exact_receipt_admits_complete_inventories_above_scope_descriptor_bound() {
        use tidepool_repr::execution_schema::{InventoryDecodeLimits, InventoryOperation};

        let directory = tempfile::tempdir().unwrap();
        let (request, context, receipt) = source_selected_receipt(directory.path(), false, None);
        let mut include = (0..512)
            .map(|index| {
                let mut root = directory.path().join(format!("absent-{index}"));
                for _ in 0..8 {
                    root.push("p".repeat(128));
                }
                root
            })
            .collect::<Vec<_>>();
        include.push(directory.path().to_path_buf());
        let request = request.with_source_search_context(&include);
        let candidates = |module: &str| {
            include
                .iter()
                .flat_map(|root| {
                    ["hs", "lhs", "hsig", "lhsig"]
                        .map(|extension| root.join(format!("{module}.{extension}")))
                })
                .collect::<Vec<_>>()
        };
        let mut value = read_receipt(&receipt);
        let fields = value.as_array_mut().unwrap();
        let mut fresh: crate::cache::DependencyEvidence =
            serde_json::from_str(fields[7].as_text().unwrap()).unwrap();
        let source = "module Consumer where\nimport A\nimport Data.List\n";
        let source_path = PathBuf::from(fields[4].as_text().unwrap());
        std::fs::write(&source_path, source).unwrap();
        std::fs::write(receipt.parent().unwrap().join("source.hs"), source).unwrap();
        fields[5] = text(sha256(source.as_bytes()));
        fresh.sources[0].sha256 = sha256(source.as_bytes());
        fresh.modules[0]
            .imports
            .push(crate::cache::ModuleImportEvidence {
                qualifier: crate::cache::ImportQualifier::Unqualified,
                module: "Data.List".into(),
                boot: false,
                selected: None,
            });
        fresh.packages.push("Data.List".into());
        fresh.resolutions.push(crate::cache::ResolutionEvidence {
            qualifier: crate::cache::ImportQualifier::Unqualified,
            module: "Data.List".into(),
            boot: false,
            selected: None,
            candidates: candidates("Data/List"),
        });
        fields[7] = text(serde_json::to_string(&fresh).unwrap());
        let selection = fields[9].as_array_mut().unwrap();
        let mut selected: crate::cache::DependencyEvidence =
            serde_json::from_str(selection[1].as_text().unwrap()).unwrap();
        let mut selected_candidates = candidates("A");
        selected_candidates.truncate(selected_candidates.len() - 3);
        selected.resolutions[0].candidates = selected_candidates;
        selection[1] = text(serde_json::to_string(&selected).unwrap());
        write_receipt(&receipt, &value);
        let bytes = std::fs::read(&receipt).unwrap();
        assert!(bytes.len() > EXACT_SCOPE_BYTES_LIMIT);
        assert!(bytes.len() < crate::certified_products::COMPILER_RECEIPT_BYTES_LIMIT);
        let admission = request.validate_receipt(&receipt, None, &context).unwrap();
        assert_eq!(admission.selected_originals.len(), 1);
        assert_eq!(admission.evidence.resolutions[0].candidates.len(), 2052);

        let restricted = InventoryOperation::new(InventoryDecodeLimits {
            max_bytes: bytes.len() - 1,
            ..Default::default()
        });
        assert!(matches!(
            decode_exact_compilation_receipt_with_operation(&receipt, None, &restricted),
            Err(CompileError::CompilerEvidence(error))
                if matches!(error.as_ref(), crate::certified_products::CertificationError::EvidenceRead {
                    path,
                    failure: crate::certified_products::EvidenceReadFailure::SizeLimit { actual, limit },
                } if path == &receipt && *actual == bytes.len() as u64 && *limit == bytes.len() as u64 - 1)
        ));
        let exhausted = InventoryOperation::new(InventoryDecodeLimits {
            max_work: 128 << 20,
            ..Default::default()
        });
        assert!(matches!(
            decode_exact_compilation_receipt_with_operation(&receipt, None, &exhausted),
            Err(CompileError::CompilerEvidence(error))
                if matches!(error.as_ref(), crate::certified_products::CertificationError::Product(
                    tidepool_repr::execution_schema::ParseError::LimitExceeded("work")
                ))
        ));
        let mut wrong_request = request.clone();
        wrong_request.request_sha256 = hex(&[99; 32]);
        assert!(wrong_request
            .validate_receipt(&receipt, None, &context)
            .is_err());
        let mut trailing = bytes.clone();
        trailing.push(0);
        std::fs::write(&receipt, trailing).unwrap();
        assert!(read_exact_compilation_receipt(&receipt).is_err());
        std::fs::write(&receipt, &bytes).unwrap();
        std::fs::write(receipt.parent().unwrap().join("source.hs"), b"changed").unwrap();
        assert!(read_exact_compilation_receipt(&receipt).is_err());
        std::fs::write(receipt.parent().unwrap().join("source.hs"), source).unwrap();
        std::fs::write(directory.path().join("A.hs"), b"changed original").unwrap();
        assert!(request.validate_receipt(&receipt, None, &context).is_err());
    }

    #[test]
    fn exact_receipt_refuses_records_above_compiler_receipt_bound() {
        let directory = tempfile::tempdir().unwrap();
        let receipt = directory.path().join("receipt.cbor");
        let limit = crate::certified_products::COMPILER_RECEIPT_BYTES_LIMIT;
        std::fs::File::create(&receipt)
            .unwrap()
            .set_len(limit as u64 + 1)
            .unwrap();
        assert!(matches!(
            read_exact_compilation_receipt(&receipt),
            Err(CompileError::CompilerEvidence(error))
                if matches!(error.as_ref(), crate::certified_products::CertificationError::EvidenceRead {
                    path,
                    failure: crate::certified_products::EvidenceReadFailure::SizeLimit { actual, limit: bound },
                } if path == &receipt && *actual == limit as u64 + 1 && *bound == limit as u64)
        ));
    }

    #[test]
    fn completed_receipt_preserves_replay_refusal_and_exact_import_authority() {
        let directory = tempfile::tempdir().unwrap();
        let context = Arc::new(ExactDeclarationContext::new(&[], &[], vec![]).unwrap());
        let request = program_request(directory.path(), context.clone());
        let receipt = import_receipt(directory.path(), &request, "Unadmitted");
        let mut value = read_receipt(&receipt);
        // Start with a complete control that claims no retained import.
        let modules = value.as_array_mut().unwrap()[8].as_array_mut().unwrap();
        modules[0].as_array_mut().unwrap()[3] = Value::Array(vec![]);
        write_receipt(&receipt, &value);
        request.validate_receipt(&receipt, None, &context).unwrap();

        let fields = value.as_array_mut().unwrap();
        let mut evidence: crate::cache::DependencyEvidence =
            serde_json::from_str(fields[7].as_text().unwrap()).unwrap();
        evidence.cache_safe = false;
        evidence.selection_complete = false;
        fields[7] = text(serde_json::to_string(&evidence).unwrap());
        write_receipt(&receipt, &value);
        let admitted = request.validate_receipt(&receipt, None, &context).unwrap();
        assert!(!admitted.evidence.cache_safe && !admitted.evidence.selection_complete);
        assert!(!admitted.evidence.valid("module Consumer where\n"));
        assert!(admitted
            .evidence
            .revalidate("module Consumer where\n")
            .is_ok());

        // Completed source observations do not authorize a retained import.
        let modules = value.as_array_mut().unwrap()[8].as_array_mut().unwrap();
        modules[0].as_array_mut().unwrap()[3] = Value::Array(vec![Value::Array(vec![
            text("none"),
            text("Unadmitted"),
            Value::Bool(false),
            text("fixture"),
        ])]);
        write_receipt(&receipt, &value);
        assert!(matches!(request.validate_receipt(&receipt, None, &context),
            Err(CompileError::ExtractFailed(detail)) if detail.contains("leaves selected lexical graph")));
    }

    #[test]
    fn receipt_observations_reject_changed_snapshots_and_trailing_bytes() {
        let directory = tempfile::tempdir().unwrap();
        let context = Arc::new(ExactDeclarationContext::new(&[], &[], vec![]).unwrap());
        let request = program_request(directory.path(), context);
        let receipt = import_receipt(directory.path(), &request, "Unadmitted");
        let expected = std::fs::read(&receipt).unwrap();
        assert!(read_exact_compilation_receipt(&receipt).is_ok());
        let snapshot = receipt.parent().unwrap().join("source.hs");
        let source = std::fs::read(&snapshot).unwrap();
        std::fs::write(&snapshot, b"module Changed where\n").unwrap();
        assert!(matches!(read_exact_compilation_receipt(&receipt),
            Err(CompileError::ExtractFailed(detail)) if detail.contains("snapshot changed")));
        std::fs::write(&snapshot, source).unwrap();
        let mut trailing = expected.clone();
        trailing.push(0);
        std::fs::write(&receipt, trailing).unwrap();
        assert!(matches!(read_exact_compilation_receipt(&receipt),
        Err(CompileError::CompilerEvidence(error))
            if matches!(error.as_ref(), crate::certified_products::CertificationError::Product(
                tidepool_repr::execution_schema::ParseError::TrailingBytes
            ))));
        std::fs::write(&receipt, expected).unwrap();
        assert!(read_exact_compilation_receipt(&receipt).is_ok());
    }

    fn source_selected_receipt(
        root: &Path,
        dependency: bool,
        shadow: Option<&Path>,
    ) -> (
        ExactCompilationRequest,
        Arc<ExactDeclarationContext>,
        PathBuf,
    ) {
        let (graph, owners) = if dependency {
            crate::execution_source::test_graph_with_local_source_dependency(root)
        } else {
            crate::execution_source::test_graph(root)
        };
        source_selected_receipt_with_graph(root, dependency, shadow, graph, owners)
    }

    fn source_selected_receipt_with_graph(
        root: &Path,
        dependency: bool,
        shadow: Option<&Path>,
        graph: Arc<crate::execution_source::CertifiedExecutionSourceGraph>,
        owners: Vec<CachedHomeOwner>,
    ) -> (
        ExactCompilationRequest,
        Arc<ExactDeclarationContext>,
        PathBuf,
    ) {
        let inventory = ArtifactInventory::default();
        // The graph's generated Input target is not an admitted original and
        // deliberately has no source replay capability. Retain only A and B.
        let originals = &owners[..2];
        assert!(originals
            .iter()
            .all(|owner| graph.eligible_source_replay_root(owner)));
        let entries = originals
            .iter()
            .map(|owner| {
                let entry = execution_entry(owner.clone(), Arc::clone(&graph));
                let ArtifactPayload::Original(product) = &entry.payload else {
                    unreachable!()
                };
                let original_imports = if dependency && owner.module == "A" {
                    vec![crate::certified_products::CanonicalSourceImport {
                        qualifier: crate::cache::ImportQualifier::Unqualified,
                        module: "B".into(),
                        boot: false,
                        home_unit: Some(owner.unit.clone()),
                    }]
                } else {
                    vec![]
                };
                let product = crate::certified_products::fixture_source_finalized_product(
                    product.clone().with_source_sha256(
                        Sha256::digest(
                            std::fs::read(root.join(format!("{}.hs", owner.module))).unwrap(),
                        )
                        .into(),
                    ),
                    [7; 32],
                    original_imports,
                )
                .with_execution_source(Arc::clone(&graph))
                .unwrap();
                Arc::new(ArtifactEntry::original([7; 32], product).unwrap())
            })
            .collect();
        let context = Arc::new(ExactDeclarationContext {
            producer: [7; 32],
            original_instance_environment: OriginalInstanceEnvironment::Unknown,
            template_imports: None,
            inventory: inventory
                .admit_shared(&inventory.empty_view(), entries)
                .unwrap(),
            lexical: vec![],
        });
        let selected_interfaces = context
            .artifact_view()
            .entries_for_owners(
                originals
                    .iter()
                    .map(|owner| identity(&owner.unit, &owner.module)),
            )
            .unwrap();
        let include = shadow
            .into_iter()
            .map(Path::to_path_buf)
            .chain(std::iter::once(root.to_path_buf()))
            .collect::<Vec<_>>();
        let mut request =
            program_request(root, Arc::clone(&context)).with_source_search_context(&include);
        request.producer_sha256 = [7; 32];
        let receipt = import_receipt_source_owner(
            root,
            &request,
            "main",
            "A",
            "none",
            false,
            "module Consumer where\nimport A\n",
        );
        let mut value = read_receipt(&receipt);
        let fields = value.as_array_mut().unwrap();
        let mut fresh: crate::cache::DependencyEvidence =
            serde_json::from_str(fields[7].as_text().unwrap()).unwrap();
        fresh.modules[0].unit = "main".into();
        fields[7] = text(serde_json::to_string(&fresh).unwrap());
        fields[8].as_array_mut().unwrap()[0].as_array_mut().unwrap()[0] = text("main");
        let count = if dependency { 2 } else { 1 };
        let mut sources = Vec::new();
        let mut modules = Vec::new();
        let mut claims = Vec::new();
        for owner in &owners[..count] {
            let source = root.join(format!("{}.hs", owner.module));
            sources.push(crate::cache::SourceEvidence {
                path: source.clone(),
                sha256: sha256(&std::fs::read(&source).unwrap()),
            });
            modules.push(crate::cache::ModuleEvidence {
                unit: owner.unit.clone(),
                module: owner.module.clone(),
                boot: false,
                source,
                product: crate::cache::ProductAvailability::InterfaceOnly,
                imports: if dependency && owner.module == "A" {
                    vec![crate::cache::ModuleImportEvidence {
                        qualifier: crate::cache::ImportQualifier::Unqualified,
                        module: "B".into(),
                        boot: false,
                        selected: Some(root.join("B.hs")),
                    }]
                } else {
                    vec![]
                },
            });
            let interface = canonical_source_interface(
                &selected_interfaces[&identity(&owner.unit, &owner.module)],
            )
            .unwrap();
            claims.push(Value::Array(vec![
                text(&owner.unit),
                text(&owner.module),
                text(sha256(interface.certificate_bytes())),
                text(hex(&interface.interface_sha256())),
                text(hex(&interface.source_sha256())),
            ]));
        }
        let mut a_candidates = if let Some(shadow) = shadow {
            vec![
                shadow.join("A.hs"),
                shadow.join("A.lhs"),
                shadow.join("A.hsig"),
                shadow.join("A.lhsig"),
            ]
        } else {
            vec![]
        };
        a_candidates.push(root.join("A.hs"));
        let mut resolutions = vec![crate::cache::ResolutionEvidence {
            qualifier: crate::cache::ImportQualifier::Unqualified,
            module: "A".into(),
            boot: false,
            selected: Some(root.join("A.hs")),
            candidates: a_candidates,
        }];
        if dependency {
            resolutions.push(crate::cache::ResolutionEvidence {
                qualifier: crate::cache::ImportQualifier::Unqualified,
                module: "B".into(),
                boot: false,
                selected: Some(root.join("B.hs")),
                candidates: vec![root.join("B.hs")],
            });
        }
        let evidence = crate::cache::DependencyEvidence {
            version: 4,
            cache_safe: true,
            selection_complete: true,
            sources,
            modules,
            resolutions,
            packages: vec![],
        };
        fields[9] = Value::Array(vec![
            Value::Array(claims),
            text(serde_json::to_string(&evidence).unwrap()),
        ]);
        write_receipt(&receipt, &value);
        (request, context, receipt)
    }

    #[test]
    fn source_selected_original_receipt_preserves_native_custody_and_requires_current_reproof() {
        let directory = tempfile::tempdir().unwrap();
        let (mut request, context, receipt) = source_selected_receipt(directory.path(), true, None);
        let admission = request.validate_receipt(&receipt, None, &context).unwrap();
        assert_eq!(admission.selected_originals.len(), 2);
        assert_eq!(
            admission.home_imports().unwrap()[&identity("main", "Consumer")],
            vec![identity("main", "A")]
        );
        assert_eq!(
            admission.home_imports().unwrap()[&identity("main", "A")],
            vec![identity("main", "B")]
        );
        let originals = context
            .artifact_view()
            .entries_for_owners([identity("main", "A"), identity("main", "B")].into_iter())
            .unwrap();
        let products =
            ["A", "B"].map(
                |module| match &originals[&identity("main", module)].payload {
                    ArtifactPayload::Original(product) => product.clone(),
                    _ => panic!("original fixture"),
                },
            );
        let effective = request
            .admit_program_support(
                Arc::clone(&context),
                &support_view(&products),
                &[admission],
                None,
            )
            .unwrap();
        let after = effective
            .artifact_view()
            .entries_for_owners([identity("main", "A"), identity("main", "B")].into_iter())
            .unwrap();
        for module in ["A", "B"] {
            assert!(Arc::ptr_eq(
                &originals[&identity("main", module)],
                &after[&identity("main", module)]
            ));
        }
        assert!(effective.lexical_graph().is_empty());
        assert!(context.lexical_graph().is_empty());
        assert_eq!(
            request.program_source_lexical(),
            &[
                ExactLexicalNode {
                    owner: identity("main", "A"),
                    imports: vec![identity("main", "B")],
                },
                ExactLexicalNode {
                    owner: identity("main", "B"),
                    imports: vec![],
                },
            ]
        );
        let (_, lexical) = effective
            .retain_value_source_surface(
                &request.program_support.as_ref().unwrap().artifacts,
                request.program_source_lexical(),
            )
            .unwrap();
        assert_eq!(lexical, request.program_source_lexical());
        assert_eq!(
            request
                .program_support
                .as_ref()
                .unwrap()
                .artifacts
                .root_entries()
                .iter()
                .map(|entry| entry.descriptor.owner.clone())
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([identity("main", "A"), identity("main", "B")])
        );
        request
            .validate_receipt(&receipt, None, &effective)
            .unwrap();
        let mut without_proof = read_receipt(&receipt);
        without_proof.as_array_mut().unwrap()[9] =
            Value::Array(vec![Value::Array(vec![]), Value::Null]);
        write_receipt(&receipt, &without_proof);
        assert!(
            request
                .validate_receipt(&receipt, None, &effective)
                .is_err(),
            "prior source-selected support must not become blanket import authority"
        );
        let (_, _, receipt) = source_selected_receipt(directory.path(), false, None);
        std::fs::write(directory.path().join("A.hs"), "module A where\nvalue = 2\n").unwrap();
        assert!(
            request
                .validate_receipt(&receipt, None, &effective)
                .is_err(),
            "a later stage must recheck current source bytes"
        );
    }

    #[test]
    fn source_selected_original_receipt_refuses_canonical_identity_version_and_unreachable_claims()
    {
        let directory = tempfile::tempdir().unwrap();
        let (request, context, receipt) = source_selected_receipt(directory.path(), false, None);
        let valid = read_receipt(&receipt);
        for index in 2..5 {
            let mut forged = valid.clone();
            forged.as_array_mut().unwrap()[9].as_array_mut().unwrap()[0]
                .as_array_mut()
                .unwrap()[0]
                .as_array_mut()
                .unwrap()[index] = text(hex(&[99; 32]));
            write_receipt(&receipt, &forged);
            assert!(
                request.validate_receipt(&receipt, None, &context).is_err(),
                "forged original digest {index}"
            );
        }
        for version in ["1", "2"] {
            let mut forged = valid.clone();
            forged.as_array_mut().unwrap()[1] = text(version);
            write_receipt(&receipt, &forged);
            assert!(request.validate_receipt(&receipt, None, &context).is_err());
        }
        let mut duplicate = valid.clone();
        let claims = duplicate.as_array_mut().unwrap()[9].as_array_mut().unwrap()[0]
            .as_array_mut()
            .unwrap();
        claims.push(claims[0].clone());
        write_receipt(&receipt, &duplicate);
        assert!(request.validate_receipt(&receipt, None, &context).is_err());
        write_receipt(&receipt, &valid);
        let mut unbound = request.clone();
        unbound.source_search_include = None;
        assert!(unbound.validate_receipt(&receipt, None, &context).is_err());
        let mut wrong_producer = request.clone();
        wrong_producer.producer_sha256 = [99; 32];
        assert!(wrong_producer
            .validate_receipt(&receipt, None, &context)
            .is_err());
        let mut unused = valid.clone();
        unused.as_array_mut().unwrap()[8].as_array_mut().unwrap()[0]
            .as_array_mut()
            .unwrap()[3] = Value::Array(vec![]);
        write_receipt(&receipt, &unused);
        assert!(
            request.validate_receipt(&receipt, None, &context).is_err(),
            "retained custody alone cannot select an unused source"
        );
    }

    #[test]
    fn source_selected_original_receipt_refuses_shadow_shortened_search_and_alternate_path() {
        let directory = tempfile::tempdir().unwrap();
        let shadow = directory.path().join("earlier");
        std::fs::create_dir(&shadow).unwrap();
        let (request, context, receipt) =
            source_selected_receipt(directory.path(), false, Some(&shadow));
        request.validate_receipt(&receipt, None, &context).unwrap();
        let valid = read_receipt(&receipt);
        for reordered in [false, true] {
            let mut forged = valid.clone();
            let fields = forged.as_array_mut().unwrap();
            let mut evidence: crate::cache::DependencyEvidence =
                serde_json::from_str(fields[9].as_array().unwrap()[1].as_text().unwrap()).unwrap();
            if reordered {
                evidence.resolutions[0].candidates.swap(0, 1);
            } else {
                evidence.resolutions[0].candidates.remove(0);
            }
            fields[9].as_array_mut().unwrap()[1] = text(serde_json::to_string(&evidence).unwrap());
            write_receipt(&receipt, &forged);
            assert!(request.validate_receipt(&receipt, None, &context).is_err());
        }
        write_receipt(&receipt, &valid);
        std::fs::write(shadow.join("A.hsig"), "signature A where\n").unwrap();
        assert!(
            request.validate_receipt(&receipt, None, &context).is_err(),
            "an earlier signature candidate also shadows the original source"
        );
        std::fs::remove_file(shadow.join("A.hsig")).unwrap();
        let mut alternate = valid;
        let fields = alternate.as_array_mut().unwrap();
        let mut evidence: crate::cache::DependencyEvidence =
            serde_json::from_str(fields[9].as_array().unwrap()[1].as_text().unwrap()).unwrap();
        let alias = directory.path().join("SameBytes.hs");
        std::fs::copy(directory.path().join("A.hs"), &alias).unwrap();
        evidence.sources[0].path = alias.clone();
        evidence.modules[0].source = alias;
        fields[9].as_array_mut().unwrap()[1] = text(serde_json::to_string(&evidence).unwrap());
        write_receipt(&receipt, &alternate);
        assert!(
            request.validate_receipt(&receipt, None, &context).is_err(),
            "equal bytes at another source path do not replace the original"
        );
    }

    #[test]
    fn source_selected_original_receipt_requires_complete_current_source_import_closure() {
        let directory = tempfile::tempdir().unwrap();
        let (request, context, receipt) = source_selected_receipt(directory.path(), true, None);
        let admission = request.validate_receipt(&receipt, None, &context).unwrap();
        assert_eq!(
            admission.home_imports().unwrap()[&identity("main", "A")],
            vec![identity("main", "B")]
        );
        assert_eq!(admission.selected_originals.len(), 2);
        let valid = read_receipt(&receipt);
        let mut missing = valid.clone();
        let fields = missing.as_array_mut().unwrap();
        let selection = fields[9].as_array_mut().unwrap();
        selection[0].as_array_mut().unwrap().pop();
        let mut evidence: crate::cache::DependencyEvidence =
            serde_json::from_str(selection[1].as_text().unwrap()).unwrap();
        evidence.sources.pop();
        evidence.modules.pop();
        selection[1] = text(serde_json::to_string(&evidence).unwrap());
        write_receipt(&receipt, &missing);
        assert!(request.validate_receipt(&receipt, None, &context).is_err());
        let mut changed = valid;
        let selection = changed.as_array_mut().unwrap()[9].as_array_mut().unwrap();
        let mut evidence: crate::cache::DependencyEvidence =
            serde_json::from_str(selection[1].as_text().unwrap()).unwrap();
        evidence.modules[0].imports.clear();
        selection[1] = text(serde_json::to_string(&evidence).unwrap());
        write_receipt(&receipt, &changed);
        assert!(
            request.validate_receipt(&receipt, None, &context).is_err(),
            "a receipt cannot omit an original source import"
        );
    }

    #[test]
    fn source_selected_canonical_receipt_admits_support_without_native_products() {
        let directory = tempfile::tempdir().unwrap();
        let (mut request, native, receipt) = source_selected_receipt(directory.path(), true, None);
        let originals = native
            .artifact_view()
            .entries_for_owners([identity("main", "A"), identity("main", "B")].into_iter())
            .unwrap();
        let inventory = ArtifactInventory::default();
        let context = Arc::new(ExactDeclarationContext {
            producer: [7; 32],
            original_instance_environment: OriginalInstanceEnvironment::Unknown,
            template_imports: None,
            lexical: vec![],
            inventory: inventory
                .admit_shared(
                    &inventory.empty_view(),
                    originals
                        .values()
                        .map(|entry| {
                            Arc::new(ArtifactEntry::canonical(
                                canonical_source_interface(entry).unwrap().clone(),
                            ))
                        })
                        .collect(),
                )
                .unwrap(),
        });
        request.context = Arc::clone(&context);
        let admission = request.validate_receipt(&receipt, None, &context).unwrap();
        let retained = request
            .admit_program_support(Arc::clone(&context), &support_view(&[]), &[admission], None)
            .unwrap();
        assert!(original_products(&retained.artifact_view().entries()).is_empty());
        assert_eq!(request.program_source_lexical().len(), 2);
        assert!(context.lexical_graph().is_empty());
        request.validate_receipt(&receipt, None, &retained).unwrap();
    }

    #[test]
    fn authored_native_root_rejects_source_original_with_authored_module_spelling() {
        let context = ExactDeclarationContext::new(&[], &[], vec![])
            .unwrap()
            .extend_checked_original_products(
                [2; 32],
                &[support_product_in_unit("main", "Tidepool.Session.Lib.G1")],
            )
            .unwrap();
        for generation in [0, 1] {
            assert!(matches!(
                context.authored_native_root(generation),
                Err(CompileError::ArtifactInventory(error))
                    if matches!(error.failure,
                        ArtifactInventoryFailure::AuthoredNativeRoot {
                            generation: actual, found: 0
                        } if actual == generation)
            ));
        }
    }

    #[test]
    fn checked_value_import_authority_keeps_hidden_and_future_owners_unselected() {
        let root = tempfile::tempdir().unwrap();
        let context = Arc::new(
            ExactDeclarationContext::new(&[], &[], vec![])
                .unwrap()
                .extend_checked_original_products([2; 32], &[support_product("Hidden")])
                .unwrap(),
        );
        let owner = tidepool_repr::SessionModule::val(tidepool_repr::Generation(7));
        let inputs = crate::checked_cell::CheckedValueInputs::capture_raw(vec![(
            owner,
            Arc::from(b"original thin interface".as_slice()),
        )])
        .unwrap();
        let request = program_request(root.path(), context.clone())
            .with_checked_value_imports(inputs.import_authority());
        request.checked_value_imports.validate().unwrap();
        let receipt = import_receipt_owner(
            root.path(),
            &request,
            "main",
            &owner.module_name(),
            "none",
            false,
        );
        assert!(request.validate_receipt(&receipt, None, &context).is_ok());
        assert!(context.lexical_graph().is_empty());
        let mut value: Value =
            ciborium::de::from_reader(std::fs::read(&receipt).unwrap().as_slice()).unwrap();
        let fields = value.as_array_mut().unwrap();
        let snapshot = PathBuf::from(fields[6].as_text().unwrap());
        let replacement = "module Tidepool.Session.Val.G7 where\n";
        std::fs::write(&snapshot, replacement).unwrap();
        fields[5] = text(sha256(replacement.as_bytes()));
        let mut evidence: crate::cache::DependencyEvidence =
            serde_json::from_str(fields[7].as_text().unwrap()).unwrap();
        evidence.sources[0].sha256 = sha256(replacement.as_bytes());
        evidence.modules[0].unit = "main".into();
        evidence.modules[0].module = owner.module_name();
        fields[7] = text(serde_json::to_string(&evidence).unwrap());
        let module = fields[8].as_array_mut().unwrap()[0].as_array_mut().unwrap();
        module[0] = text("main");
        module[1] = text(owner.module_name());
        module[3] = Value::Array(vec![]);
        let mut collision = Vec::new();
        ciborium::ser::into_writer(&value, &mut collision).unwrap();
        std::fs::write(&receipt, collision).unwrap();
        assert!(request
            .validate_receipt(&receipt, None, &context)
            .err()
            .expect("source owner collision must fail")
            .to_string()
            .contains("fresh module replaced an admitted exact owner"));
        for (unit, module, qualifier, boot) in [
            ("fixture", "Hidden", "none", false),
            ("main", "Tidepool.Session.Val.G8", "none", false),
            ("foreign", "Tidepool.Session.Val.G7", "none", false),
            ("main", "Tidepool.Session.Val.G7", "other:main", false),
            ("main", "Tidepool.Session.Val.G7", "none", true),
        ] {
            let receipt =
                import_receipt_owner(root.path(), &request, unit, module, qualifier, boot);
            assert!(
                request.validate_receipt(&receipt, None, &context).is_err(),
                "{unit}:{module} {qualifier} boot={boot}"
            );
        }
        std::fs::write(
            inputs.root().join(owner.relative_hi_path()),
            b"changed thin interface",
        )
        .unwrap();
        assert!(request.checked_value_imports.validate().is_err());
    }

    const RECEIVER_INTERFACE_SOURCE: &str = "module Tidepool.Agent.Ref where\n";

    fn interface_only_agent_ref_admission(root: &Path, unit: &str) -> ExactSourceAdmission {
        let source = RECEIVER_INTERFACE_SOURCE;
        let path = root.join("Ref.hs");
        std::fs::write(&path, source).unwrap();
        let mut admission = support_admission(root);
        let mut evidence = admission.evidence.into_evidence();
        evidence
            .sources
            .retain(|source| source.path == Path::new(crate::cache::GENERATED_SOURCE));
        evidence.sources.push(crate::cache::SourceEvidence {
            path: path.clone(),
            sha256: sha256(source.as_bytes()),
        });
        evidence.resolutions.clear();
        evidence.modules = vec![crate::cache::ModuleEvidence {
            unit: unit.into(),
            module: "Tidepool.Agent.Ref".into(),
            boot: false,
            source: path,
            imports: vec![],
            product: crate::cache::ProductAvailability::InterfaceOnly,
        }];
        admission.evidence_bytes = serde_json::to_vec(&evidence).unwrap();
        admission.evidence = crate::cache::CompletedSourceEvidence::from_normalized(
            evidence,
            "module Target where\n",
        )
        .unwrap();
        admission
    }

    fn interface_only_agent_ref(producer: [u8; 32], unit: &str) -> ArtifactView {
        let interface = crate::certified_products::fixture_source_module_interface(
            producer,
            unit,
            "Tidepool.Agent.Ref",
            Sha256::digest(RECEIVER_INTERFACE_SOURCE.as_bytes()).into(),
            BTreeMap::new(),
            None,
        );
        certified_product_artifact_view(producer, &[], &[interface], None).unwrap()
    }

    fn receiver_value_interface() -> Arc<CertifiedValueInterface> {
        let evidence = support_product_in_unit("main", "Tidepool.Session.Val.G1");
        Arc::new(
            CertifiedValueInterface::from_checked_compilation(
                [2; 32],
                identity("main", "Tidepool.Session.Val.G1"),
                evidence.interface_bytes().to_vec(),
                evidence.package_imports_bytes().to_vec(),
                vec![identity("main", "Tidepool.Agent.Ref")],
            )
            .unwrap(),
        )
    }

    #[test]
    fn program_support_preserves_interface_only_receiver_dependency_without_native_authority() {
        let directory = tempfile::tempdir().unwrap();
        let baseline = Arc::new(ExactDeclarationContext::new(&[], &[], vec![]).unwrap());
        let mut request = program_request(directory.path(), baseline.clone());
        let support = interface_only_agent_ref([2; 32], "main");
        let context = request
            .admit_program_support(
                baseline,
                &support,
                &[interface_only_agent_ref_admission(directory.path(), "main")],
                None,
            )
            .unwrap();
        let owner = identity("main", "Tidepool.Agent.Ref");
        assert!(context.recovery_products().is_empty());
        assert!(context.lexical_graph().is_empty());
        assert!(!context
            .artifact_view()
            .source_implementation_roles()
            .contains_key(&owner));
        let entry = context
            .artifact_view()
            .entries_for_owners(std::iter::once(owner.clone()))
            .unwrap();
        assert_eq!(
            entry[&owner].descriptor.kind,
            crate::artifact_inventory::ArtifactKind::CanonicalModuleInterface
        );
        assert_eq!(
            request
                .program_support
                .as_ref()
                .unwrap()
                .artifacts
                .artifact_ids(),
            support.artifact_ids()
        );
        // These are the two value-interface consumers after checked/program support.
        for context in [
            (*context)
                .clone()
                .extend_with_value_interfaces(&[receiver_value_interface()], vec![])
                .unwrap(),
            (*context)
                .clone()
                .extend_program_value_interface(receiver_value_interface())
                .unwrap(),
        ] {
            assert!(context.recovery_products().is_empty());
            assert!(!context
                .lexical_graph()
                .iter()
                .any(|node| node.owner == owner));
            assert!(context.artifact_view().interface_dependencies().iter().any(
                |(_, required, dependency)| *required == entry[&owner].descriptor.id
                    && matches!(
                        dependency,
                        crate::artifact_inventory::ArtifactDependency::Interface
                    )
            ));
        }
    }

    #[test]
    fn program_support_refuses_fresh_value_type_without_reserved_output_authority() {
        let directory = tempfile::tempdir().unwrap();
        let baseline = Arc::new(ExactDeclarationContext::new(&[], &[], vec![]).unwrap());
        let mut request = program_request(directory.path(), baseline.clone());
        let context = request
            .admit_program_support(
                baseline,
                &interface_only_agent_ref([2; 32], "main"),
                &[interface_only_agent_ref_admission(directory.path(), "main")],
                None,
            )
            .unwrap();
        let value = receiver_value_interface();
        let candidate = (*context)
            .clone()
            .extend_program_value_interface(value.clone())
            .unwrap();
        let support = candidate
            .artifact_view()
            .select_roots(vec![value.artifact_id()])
            .unwrap();
        let previous = request
            .program_support
            .as_ref()
            .unwrap()
            .artifacts
            .artifact_ids();
        let refusal = request
            .admit_program_support(context.clone(), &support, &[], None)
            .expect_err("a type certificate alone cannot supply fresh source or reserved outputs");
        assert!(matches!(refusal, CompileError::ExtractFailed(_)));
        assert_eq!(
            request
                .program_support
                .as_ref()
                .unwrap()
                .artifacts
                .artifact_ids(),
            previous
        );
        assert!(!context
            .artifact_view()
            .artifact_ids()
            .contains(&value.artifact_id()));
        assert!(context.lexical_graph().is_empty());
        assert!(context.recovery_products().is_empty());
    }

    #[test]
    fn interface_only_receiver_support_refuses_unavailable_wrong_owner_and_unadmitted_source() {
        let directory = tempfile::tempdir().unwrap();
        let empty = Arc::new(ExactDeclarationContext::new(&[], &[], vec![]).unwrap());
        let assert_missing_ref = |error: CompileError| {
            assert!(matches!(error, CompileError::ArtifactInventory(error)
                if matches!(&error.failure, ArtifactInventoryFailure::MissingDependency {
                    dependent, required, dependency: crate::artifact_inventory::ArtifactDependency::Interface, ..
                } if dependent == &identity("main", "Tidepool.Session.Val.G1")
                    && required == &identity("main", "Tidepool.Agent.Ref"))));
        };
        assert_missing_ref(
            (*empty)
                .clone()
                .extend_program_value_interface(receiver_value_interface())
                .unwrap_err(),
        );
        let mut request = program_request(directory.path(), empty.clone());
        let wrong_owner = request
            .admit_program_support(
                empty.clone(),
                &interface_only_agent_ref([2; 32], "foreign"),
                &[interface_only_agent_ref_admission(
                    directory.path(),
                    "foreign",
                )],
                None,
            )
            .unwrap();
        assert_missing_ref(
            (*wrong_owner)
                .clone()
                .extend_program_value_interface(receiver_value_interface())
                .unwrap_err(),
        );
        let support = interface_only_agent_ref([2; 32], "main");
        let hidden = Arc::new(
            (*empty)
                .clone()
                .extend_interface_artifacts(&support)
                .unwrap(),
        );
        let mut request = program_request(directory.path(), hidden.clone());
        assert!(request
            .admit_program_support(
                hidden.clone(),
                &support,
                &[interface_only_agent_ref_admission(directory.path(), "main")],
                None,
            )
            .is_err());
        assert!(hidden.lexical_graph().is_empty());
        assert!(request.program_support.is_none());
        let uncaptured = interface_only_agent_ref_admission(directory.path(), "main");
        let mut uncaptured = uncaptured.evidence.into_evidence();
        uncaptured.modules[0].source = directory.path().join("uncaptured.hs");
        assert!(crate::cache::CompletedSourceEvidence::from_normalized(
            uncaptured,
            "module Target where\n",
        )
        .is_err());
        for (case, producer, admissions) in [
            ("unadmitted source", [2; 32], vec![]),
            (
                "different producer",
                [3; 32],
                vec![interface_only_agent_ref_admission(directory.path(), "main")],
            ),
        ] {
            let mut request = program_request(directory.path(), empty.clone());
            assert!(
                request
                    .admit_program_support(
                        empty.clone(),
                        &interface_only_agent_ref(producer, "main"),
                        &admissions,
                        None
                    )
                    .is_err(),
                "{case}"
            );
            assert!(empty.artifact_view().is_empty(), "{case}");
            assert!(request.program_support.is_none(), "{case}");
        }
    }

    #[test]
    fn program_support_retains_selected_home_edges_without_public_exposure() {
        let directory = tempfile::tempdir().unwrap();
        let hidden = support_product("Hidden");
        let baseline = Arc::new(
            ExactDeclarationContext::new(&[], &[], vec![])
                .unwrap()
                .extend_checked_original_products([2; 32], &[hidden])
                .unwrap(),
        );
        let mut request = program_request(directory.path(), baseline.clone());
        let admission = support_admission(directory.path());
        let context = request
            .admit_program_support(
                baseline.clone(),
                &support_view(&[
                    support_product("InstanceOwner"),
                    support_product("InstanceRelay"),
                ]),
                &[admission],
                None,
            )
            .unwrap();
        assert!(context.lexical_graph().is_empty());
        assert!(baseline.lexical_graph().is_empty());
        let relay = context
            .artifact_view()
            .entries_for_owners(std::iter::once(identity("fixture", "InstanceRelay")))
            .unwrap();
        // The source import selects lexical support; this fixture's canonical
        // interface deliberately has no type dependency on InstanceOwner.
        assert!(relay[&identity("fixture", "InstanceRelay")]
            .requirements
            .is_empty());
        assert_eq!(
            request.program_source_lexical(),
            &[
                ExactLexicalNode {
                    owner: identity("fixture", "InstanceOwner"),
                    imports: vec![]
                },
                ExactLexicalNode {
                    owner: identity("fixture", "InstanceRelay"),
                    imports: vec![identity("fixture", "InstanceOwner")]
                },
            ]
        );
        let effective = request
            .in_program_context(&directory.path().join("program-inputs"), context)
            .unwrap();
        let receipt = import_receipt(directory.path(), &effective, "InstanceRelay");
        let admission = effective
            .validate_receipt(&receipt, None, &effective.context)
            .unwrap();
        let artifacts = effective
            .context
            .artifact_view()
            .merge(&support_view(&[support_product("Consumer")]))
            .unwrap();
        let original = ExactProductAdmission {
            request: &effective,
            source: &admission,
        }
        .original_execution_context(&artifacts)
        .unwrap();
        assert!(matches!(original.original_instance_environment(),
            OriginalInstanceEnvironment::Complete { target }
                if target == &identity("fixture", "Consumer")));
        assert_eq!(
            original
                .lexical_graph()
                .iter()
                .find(|node| node.owner == identity("fixture", "InstanceRelay"))
                .unwrap()
                .imports,
            vec![identity("fixture", "InstanceOwner")]
        );
        assert!(original
            .lexical_graph()
            .iter()
            .any(|node| node.owner == identity("fixture", "InstanceOwner")));
        assert!(!original
            .lexical_graph()
            .iter()
            .any(|node| node.owner == identity("fixture", "Hidden")));
        assert!(effective.context.lexical_graph().is_empty());
        let mut incomplete = effective.clone();
        Arc::make_mut(&mut incomplete.program_support.as_mut().unwrap().imports)
            .remove(&identity("fixture", "InstanceOwner"));
        let incomplete = ExactProductAdmission {
            request: &incomplete,
            source: &admission,
        }
        .original_execution_context(&artifacts)
        .unwrap();
        assert!(
            matches!(incomplete.original_instance_environment(),
            OriginalInstanceEnvironment::MissingOriginalOwners(owners)
                if owners.contains(&identity("fixture", "InstanceOwner"))),
            "retained interface custody cannot invent an absent original import row"
        );
        let receipt = import_receipt(directory.path(), &effective, "Hidden");
        assert!(effective
            .validate_receipt(&receipt, None, &effective.context)
            .is_err());
    }

    #[test]
    fn program_support_refuses_retained_hidden_owner_and_forged_source() {
        let directory = tempfile::tempdir().unwrap();
        let products = [
            support_product("InstanceOwner"),
            support_product("InstanceRelay"),
        ];
        let baseline = Arc::new(
            ExactDeclarationContext::new(&[], &[], vec![])
                .unwrap()
                .extend_checked_original_products([2; 32], &products)
                .unwrap(),
        );
        let mut request = program_request(directory.path(), baseline.clone());
        assert!(request
            .admit_program_support(
                baseline,
                &support_view(&products),
                &[support_admission(directory.path())],
                None
            )
            .is_err());
        let original = support_admission(directory.path());
        let mut forged = original.evidence.into_evidence();
        forged.modules[0].source = directory.path().join("AnotherOwner.hs");
        assert!(crate::cache::CompletedSourceEvidence::from_normalized(
            forged,
            "module Target where\n",
        )
        .is_err());
    }

    #[test]
    fn program_support_refuses_conflicting_selected_edges() {
        let directory = tempfile::tempdir().unwrap();
        let empty = Arc::new(ExactDeclarationContext::new(&[], &[], vec![]).unwrap());
        let mut request = program_request(directory.path(), empty.clone());
        let original = support_admission(directory.path());
        let mut changed = support_admission(directory.path());
        let mut evidence = changed.evidence.into_evidence();
        evidence.modules[1].imports.clear();
        changed.evidence = crate::cache::CompletedSourceEvidence::from_normalized(
            evidence,
            "module Target where\n",
        )
        .unwrap();
        assert!(request
            .admit_program_support(
                empty,
                &support_view(&[
                    support_product("InstanceOwner"),
                    support_product("InstanceRelay")
                ]),
                &[original, changed],
                None
            )
            .is_err());
    }

    #[test]
    fn program_support_dependency_closure_does_not_select_hidden_names() {
        let directory = tempfile::tempdir().unwrap();
        let hidden = support_product("Hidden");
        let relay = crate::certified_products::fixture_finalized_product_with_requirements(
            support_product("InstanceRelay"),
            [2; 32],
            Some(BTreeMap::from([(
                ("fixture".into(), "Hidden".into()),
                hidden.module_interface().unwrap().interface_sha256(),
            )])),
        );
        let context = Arc::new(
            ExactDeclarationContext::new(&[], &[], vec![])
                .unwrap()
                .extend_checked_original_products([2; 32], &[hidden, relay])
                .unwrap(),
        );
        let entries = context
            .artifact_view()
            .entries_for_owners(std::iter::once(identity("fixture", "InstanceRelay")))
            .unwrap();
        let mut request = program_request(directory.path(), context.clone());
        request.program_support = Some(ProgramSourceSupport {
            artifacts: context
                .artifact_view()
                .select_roots(vec![
                    entries[&identity("fixture", "InstanceRelay")].descriptor.id,
                ])
                .unwrap(),
            imports: Arc::new(BTreeMap::new()),
        });
        let support = &request.program_support.as_ref().unwrap().artifacts;
        let mut actual_owners = support
            .descriptors()
            .into_iter()
            .map(|descriptor| (descriptor.owner, descriptor.kind))
            .collect::<Vec<_>>();
        actual_owners.sort();
        assert_eq!(
            actual_owners,
            vec![
                (
                    identity("fixture", "Hidden"),
                    ArtifactKind::CanonicalModuleInterface
                ),
                (
                    identity("fixture", "InstanceRelay"),
                    ArtifactKind::OriginalModule
                ),
                (
                    identity("fixture", "InstanceRelay"),
                    ArtifactKind::CanonicalModuleInterface
                ),
            ]
        );
        assert_eq!(
            support
                .root_entries()
                .iter()
                .map(|entry| entry.descriptor.owner.clone())
                .collect::<Vec<_>>(),
            vec![identity("fixture", "InstanceRelay")]
        );
        assert!(!support
            .source_implementation_roles()
            .contains_key(&identity("fixture", "Hidden")));
        assert_eq!(
            entries[&identity("fixture", "InstanceRelay")].requirements,
            vec![identity("fixture", "Hidden")]
        );
        let receipt = import_receipt(directory.path(), &request, "InstanceRelay");
        assert!(request.validate_receipt(&receipt, None, &context).is_ok());
        let receipt = import_receipt(directory.path(), &request, "Hidden");
        assert!(request.validate_receipt(&receipt, None, &context).is_err());
        assert!(context.lexical_graph().is_empty());
        let (retained, lexical) = context
            .retain_value_source_surface(&request.program_support.as_ref().unwrap().artifacts, &[])
            .unwrap();
        assert!(support
            .artifact_ids()
            .iter()
            .all(|id| retained.artifact_ids().contains(id)));
        assert!(retained
            .descriptors()
            .iter()
            .any(|entry| entry.owner == identity("fixture", "Hidden")
                && entry.kind == ArtifactKind::OriginalModule));
        assert!(lexical.is_empty());
    }

    #[test]
    fn retained_value_source_surface_preserves_selected_support_and_refuses_hidden_owners() {
        let directory = tempfile::tempdir().unwrap();
        let baseline = Arc::new(
            ExactDeclarationContext::new(&[], &[], vec![])
                .unwrap()
                .extend_checked_original_products([2; 32], &[support_product("Hidden")])
                .unwrap(),
        );
        let mut request = program_request(directory.path(), baseline.clone());
        let context = request
            .admit_program_support(
                baseline,
                &support_view(&[
                    support_product("InstanceOwner"),
                    support_product("InstanceRelay"),
                ]),
                &[support_admission(directory.path())],
                None,
            )
            .unwrap();
        assert!(context.lexical_graph().is_empty());
        let owner = identity("fixture", "InstanceRelay");
        let entries = context
            .artifact_view()
            .entries_for_owners(std::iter::once(owner.clone()))
            .unwrap();
        let value = context
            .artifact_view()
            .select_roots(vec![entries[&owner].descriptor.id])
            .unwrap();
        let (view, lexical) = context
            .retain_value_source_surface(&value, request.program_source_lexical())
            .unwrap();
        assert!(lexical
            .iter()
            .all(|node| node.owner != identity("fixture", "Hidden")));
        let typed_owners = |view: &ArtifactView| {
            let mut owners = view
                .descriptors()
                .into_iter()
                .map(|entry| (entry.owner, entry.kind))
                .collect::<Vec<_>>();
            owners.sort();
            owners
        };
        let expected_owners = vec![
            (identity("fixture", "Hidden"), ArtifactKind::OriginalModule),
            (
                identity("fixture", "Hidden"),
                ArtifactKind::CanonicalModuleInterface,
            ),
            (
                identity("fixture", "InstanceOwner"),
                ArtifactKind::OriginalModule,
            ),
            (
                identity("fixture", "InstanceOwner"),
                ArtifactKind::CanonicalModuleInterface,
            ),
            (
                identity("fixture", "InstanceRelay"),
                ArtifactKind::OriginalModule,
            ),
            (
                identity("fixture", "InstanceRelay"),
                ArtifactKind::CanonicalModuleInterface,
            ),
        ];
        assert_eq!(typed_owners(&view), expected_owners);

        let later = (*context)
            .clone()
            .extend_checked_original_products([2; 32], &[support_product("LaterSupport")])
            .unwrap();
        let mut later_support = request.program_source_lexical().to_vec();
        later_support.push(ExactLexicalNode {
            owner: identity("fixture", "LaterSupport"),
            imports: vec![],
        });
        let (earlier_view, earlier_lexical) = later
            .retain_value_source_surface(&value, &later_support)
            .unwrap();
        assert!(earlier_view
            .descriptors()
            .iter()
            .any(|entry| entry.owner == identity("fixture", "LaterSupport")
                && entry.kind == ArtifactKind::OriginalModule));
        assert_eq!(
            typed_owners(&view),
            expected_owners,
            "the already captured view remains immutable"
        );
        assert_eq!(earlier_lexical, lexical);
        assert_eq!(
            lexical,
            vec![
                ExactLexicalNode {
                    owner: identity("fixture", "InstanceOwner"),
                    imports: vec![]
                },
                ExactLexicalNode {
                    owner,
                    imports: vec![identity("fixture", "InstanceOwner")]
                },
            ]
        );
        let mut following = (*context).clone();
        following.lexical = lexical.clone();
        following.normalize().unwrap();
        let following = Arc::new(following);
        let next = program_request(directory.path(), following.clone());
        assert!(next
            .validate_receipt(
                &import_receipt(directory.path(), &next, "InstanceRelay"),
                None,
                &following
            )
            .is_ok());
        assert!(next
            .validate_receipt(
                &import_receipt(directory.path(), &next, "Hidden"),
                None,
                &following
            )
            .is_err());
        // A subsequent output keeps the already selected source surface even
        // when that request has no new source-selected supporting products.
        assert_eq!(
            following
                .retain_value_source_surface(&value, &[])
                .unwrap()
                .1,
            lexical
        );
        let conflict = [ExactLexicalNode {
            owner: identity("fixture", "InstanceRelay"),
            imports: vec![],
        }];
        assert!(following
            .retain_value_source_surface(&value, &conflict)
            .is_err());
        let incomplete = [ExactLexicalNode {
            owner: identity("fixture", "InstanceRelay"),
            imports: vec![identity("fixture", "Hidden")],
        }];
        assert!(context
            .retain_value_source_surface(&value, &incomplete)
            .is_err());
        let receipt = import_receipt(directory.path(), &next, "InstanceRelay");
        let mut replacement: Value =
            ciborium::de::from_reader(std::fs::read(&receipt).unwrap().as_slice()).unwrap();
        let fields = replacement.as_array_mut().unwrap();
        let replacement_source = "module InstanceRelay where\nreplaced = 99 :: Int\n";
        std::fs::write(fields[6].as_text().unwrap(), replacement_source).unwrap();
        fields[5] = text(sha256(replacement_source.as_bytes()));
        let mut evidence: crate::cache::DependencyEvidence =
            serde_json::from_str(fields[7].as_text().unwrap()).unwrap();
        evidence.sources[0].sha256 = sha256(replacement_source.as_bytes());
        evidence.modules[0].module = "InstanceRelay".into();
        fields[7] = text(serde_json::to_string(&evidence).unwrap());
        let module = fields[8].as_array_mut().unwrap()[0].as_array_mut().unwrap();
        module[1] = text("InstanceRelay");
        module[3] = Value::Array(vec![]);
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(&replacement, &mut bytes).unwrap();
        std::fs::write(&receipt, bytes).unwrap();
        let refused = next
            .validate_receipt(&receipt, None, &following)
            .err()
            .unwrap();
        assert!(refused
            .to_string()
            .contains("fresh module replaced an admitted exact owner"));
    }

    #[test]
    fn program_support_partitions_inherited_products_from_new_selected_roots() {
        let directory = tempfile::tempdir().unwrap();
        let hidden = support_product("Hidden");
        let owner = support_product("InstanceOwner");
        let relay = support_product("InstanceRelay");
        let baseline = Arc::new(
            ExactDeclarationContext::new(&[], &[], vec![])
                .unwrap()
                .extend_checked_original_products([2; 32], std::slice::from_ref(&hidden))
                .unwrap(),
        );
        let mut request = program_request(directory.path(), baseline.clone());
        let context = request
            .admit_program_support(
                baseline,
                &support_view(&[hidden.clone(), owner.clone(), relay.clone()]),
                &[support_admission(directory.path())],
                None,
            )
            .unwrap();
        assert_eq!(
            request
                .program_support
                .as_ref()
                .unwrap()
                .artifacts
                .root_entries()
                .len(),
            2
        );
        let mut request = request
            .in_program_context(&directory.path().join("program-inputs"), context.clone())
            .unwrap();
        let path = directory.path().join("Additional.hs");
        std::fs::write(&path, b"module Additional where\n").unwrap();
        let mut later = support_admission(directory.path());
        let mut evidence = later.evidence.into_evidence();
        evidence.modules = vec![crate::cache::ModuleEvidence {
            unit: "fixture".into(),
            module: "Additional".into(),
            boot: false,
            source: path.clone(),
            imports: vec![],
            product: crate::cache::ProductAvailability::Ready,
        }];
        evidence.sources = vec![crate::cache::SourceEvidence {
            path: path.clone(),
            sha256: sha256(b"module Additional where\n"),
        }];
        evidence.resolutions.clear();
        later.witness = ExactSourceWitness {
            source_path: path.clone(),
            source_sha256: Sha256::digest(b"module Additional where\n").into(),
        };
        later.evidence_bytes = serde_json::to_vec(&evidence).unwrap();
        later.evidence = crate::cache::CompletedSourceEvidence::from_worker_evidence(
            evidence,
            &path,
            "module Additional where\n",
        )
        .unwrap();
        later.exact_imports.insert(
            identity("fixture", "Additional"),
            vec![identity("fixture", "InstanceRelay")],
        );
        let context = request
            .admit_program_support(
                context,
                &support_view(&[hidden.clone(), owner, relay, support_product("Additional")]),
                &[later],
                None,
            )
            .unwrap();
        assert_eq!(
            request
                .program_support
                .as_ref()
                .unwrap()
                .artifacts
                .root_entries()
                .len(),
            3
        );
        assert!(context.lexical_graph().is_empty());
        let effective = request
            .in_program_context(&directory.path().join("program-inputs"), context.clone())
            .unwrap();
        let receipt = import_receipt(directory.path(), &effective, "Additional");
        assert!(effective.validate_receipt(&receipt, None, &context).is_ok());
        let receipt = import_receipt(directory.path(), &effective, "Hidden");
        assert!(effective
            .validate_receipt(&receipt, None, &context)
            .is_err());
        let altered = support_product_with_interface(
            "fixture",
            "Hidden",
            b"different inherited interface".to_vec(),
        );
        assert!(request
            .admit_program_support(context, &support_view(&[altered]), &[], None)
            .is_err());
    }

    #[test]
    fn program_support_refuses_same_canonical_interface_with_another_native_owner() {
        let native_variant = |product: &CertifiedRecoveryProduct| {
            let interface = product.module_interface().unwrap().clone();
            let mut owner = product.owner().clone();
            owner.module_version = ModuleVersion([99; 32]);
            let certification = crate::certified_products::encode_home_certification_with_module(
                &owner,
                &[],
                &BTreeMap::new(),
                interface.requirements(),
                Sha256::digest(interface.certificate_bytes()).into(),
            )
            .unwrap();
            CertifiedRecoveryProduct::from_certification(
                owner,
                product.interface_bytes().to_vec(),
                product.product_bytes().to_vec(),
                product.package_imports_bytes().to_vec(),
                certification,
            )
            .with_module_interface(interface)
            .unwrap()
        };
        let directory = tempfile::tempdir().unwrap();
        let original = support_product("Hidden");
        let variant = native_variant(&original);
        assert_eq!(variant.module_interface(), original.module_interface());
        assert_ne!(variant.owner(), original.owner());
        let context = Arc::new(
            ExactDeclarationContext::new(&[], &[], vec![])
                .unwrap()
                .extend_checked_original_products([2; 32], &[original])
                .unwrap(),
        );
        let mut request = program_request(directory.path(), context.clone());
        let support = support_view(&[variant]);
        let assert_native_refusal = |error| {
            assert!(matches!(error, CompileError::ExtractFailed(detail)
                if detail == "exact declaration context: supporting original differs from retained owner"));
        };
        assert_native_refusal(
            request
                .admit_program_support(context.clone(), &support, &[], None)
                .unwrap_err(),
        );
        assert!(request.program_support.is_none());
        assert!(request.program_source_lexical().is_empty());
        assert_eq!(context.recovery_products().len(), 1);
        let unchanged = context.recovery_products();
        let unchanged_support = support_view(&unchanged);
        let unchanged_context = request
            .admit_program_support(context.clone(), &unchanged_support, &[], None)
            .unwrap();
        assert_eq!(
            unchanged_context.recovery_products()[0].owner(),
            unchanged[0].owner()
        );
        let canonical = certified_product_artifact_view(
            [2; 32],
            &[],
            &[unchanged[0].module_interface().unwrap().clone()],
            None,
        )
        .unwrap();
        let canonical_context = Arc::new(
            ExactDeclarationContext::new(&[], &[], vec![])
                .unwrap()
                .extend_interface_artifacts(&canonical)
                .unwrap(),
        );
        let mut canonical_request = program_request(directory.path(), canonical_context.clone());
        let promoted = canonical_request
            .admit_program_support(canonical_context, &unchanged_support, &[], None)
            .unwrap();
        assert_eq!(
            promoted.recovery_products()[0].owner(),
            unchanged[0].owner()
        );
        assert!(promoted.lexical_graph().is_empty());

        // A real validated current-source receipt cannot replace native identity
        // merely because the canonical interface agrees.
        let (mut request, context, receipt) = source_selected_receipt(directory.path(), true, None);
        let admission = request.validate_receipt(&receipt, None, &context).unwrap();
        let products = context.recovery_products();
        let support = support_view(
            &products
                .iter()
                .map(|product| {
                    if product.owner().module == "A" {
                        native_variant(product)
                    } else {
                        product.clone()
                    }
                })
                .collect::<Vec<_>>(),
        );
        assert_native_refusal(
            request
                .admit_program_support(context.clone(), &support, &[admission], None)
                .unwrap_err(),
        );
        assert!(request.program_support.is_none());
        assert!(request.program_source_lexical().is_empty());
        assert_eq!(context.recovery_products().len(), 2);
    }

    #[test]
    fn retained_value_source_surface_does_not_promote_compiler_scaffold_support() {
        let directory = tempfile::tempdir().unwrap();
        let baseline = Arc::new(ExactDeclarationContext::new(&[], &[], vec![]).unwrap());
        let mut request = program_request(directory.path(), baseline.clone());
        let mut admission = support_admission(directory.path());
        let mut evidence = admission.evidence.into_evidence();
        evidence.modules.truncate(1);
        let source = &mut evidence.modules[0];
        source.unit = "main".into();
        source.module = "Tidepool.Internal.Resume".into();
        source.imports.clear();
        admission.evidence = crate::cache::CompletedSourceEvidence::from_normalized(
            evidence,
            "module Target where\n",
        )
        .unwrap();
        let context = request
            .admit_program_support(
                baseline,
                &support_view(&[support_product_in_unit("main", "Tidepool.Internal.Resume")]),
                &[admission],
                None,
            )
            .unwrap();
        assert!(request.program_source_lexical().is_empty());
        let native = &request.program_support.as_ref().unwrap().artifacts;
        assert_eq!(native.descriptors().len(), 2);
        assert!(context
            .retain_value_source_surface(native, request.program_source_lexical())
            .unwrap()
            .1
            .is_empty());
        let current = request
            .in_program_context(&directory.path().join("program-inputs"), context.clone())
            .unwrap();
        let receipt = import_receipt_owner(
            directory.path(),
            &current,
            "main",
            "Tidepool.Internal.Resume",
            "none",
            false,
        );
        let admission = current.validate_receipt(&receipt, None, &context).unwrap();
        let artifacts = context
            .artifact_view()
            .merge(&support_view(&[support_product("Consumer")]))
            .unwrap();
        let original = ExactProductAdmission {
            request: &current,
            source: &admission,
        }
        .original_execution_context(&artifacts)
        .unwrap();
        assert!(matches!(
            original.original_instance_environment(),
            OriginalInstanceEnvironment::Complete { .. }
        ));
        assert!(original
            .lexical_graph()
            .iter()
            .any(|node| node.owner == identity("main", "Tidepool.Internal.Resume")));
        assert!(context.lexical_graph().is_empty());
        let next = program_request(directory.path(), context.clone());
        let receipt = import_receipt_owner(
            directory.path(),
            &next,
            "main",
            "Tidepool.Internal.Resume",
            "none",
            false,
        );
        assert!(next.validate_receipt(&receipt, None, &context).is_err());
    }

    #[test]
    fn program_support_refuses_ambiguous_selected_source_owner() {
        let directory = tempfile::tempdir().unwrap();
        let admission = support_admission(directory.path());
        let mut evidence = admission.evidence.into_evidence();
        evidence.modules.push(evidence.modules[0].clone());
        assert!(consumed_source_home_imports(&evidence, &admission.exact_imports).is_err());
        assert!(crate::cache::CompletedSourceEvidence::from_normalized(
            evidence,
            "module Target where\n",
        )
        .is_err());
    }

    #[test]
    fn program_values_retain_private_type_owners_without_selecting_their_names() {
        let directory = tempfile::tempdir().unwrap();
        let baseline = Arc::new(
            ExactDeclarationContext::new(&[], &[], vec![])
                .unwrap()
                .extend_checked_original_products([2; 32], &[support_product("Public")])
                .unwrap(),
        );
        let mut baseline = (*baseline).clone();
        baseline.lexical.push(ExactLexicalNode {
            owner: identity("fixture", "Public"),
            imports: vec![],
        });
        let baseline = Arc::new(baseline);
        let mut request = program_request(directory.path(), baseline.clone());
        let context = request
            .admit_program_support(
                baseline,
                &support_view(&[
                    support_product("InstanceOwner"),
                    support_product("InstanceRelay"),
                ]),
                &[support_admission(directory.path())],
                None,
            )
            .unwrap();
        let value = |module: &str, requirements| {
            let evidence = support_product(module);
            Arc::new(
                CertifiedValueInterface::from_checked_compilation(
                    [2; 32],
                    identity("fixture", module),
                    evidence.interface_bytes().to_vec(),
                    evidence.package_imports_bytes().to_vec(),
                    requirements,
                )
                .unwrap(),
            )
        };
        let first = value(
            "ValFirst",
            vec![
                identity("fixture", "Public"),
                identity("fixture", "InstanceRelay"),
            ],
        );
        let context = (*context)
            .clone()
            .extend_program_value_interface(first)
            .unwrap();
        assert_eq!(
            context.lexical_graph()[1].imports,
            vec![identity("fixture", "Public")]
        );
        let entry = context
            .artifact_view()
            .entries_for_owners(std::iter::once(identity("fixture", "ValFirst")))
            .unwrap();
        assert_eq!(
            entry[&identity("fixture", "ValFirst")].requirements,
            vec![
                identity("fixture", "InstanceRelay"),
                identity("fixture", "Public")
            ]
        );
        let second = value(
            "ValSecond",
            vec![
                identity("fixture", "ValFirst"),
                identity("fixture", "InstanceOwner"),
            ],
        );
        let context = context.extend_program_value_interface(second).unwrap();
        assert_eq!(
            context.lexical_graph()[2].imports,
            vec![identity("fixture", "ValFirst")]
        );
        assert!(!context
            .lexical_graph()
            .iter()
            .any(|node| node.owner.module.starts_with("Instance")));
        let forged = value("ValForged", vec![identity("fixture", "Absent")]);
        let error = context.extend_program_value_interface(forged).unwrap_err();
        assert!(matches!(error, CompileError::ArtifactInventory(error)
            if matches!(&error.failure, crate::artifact_inventory::ArtifactInventoryFailure::MissingDependency {
                required, dependency: crate::artifact_inventory::ArtifactDependency::Interface, ..
            } if required == &identity("fixture", "Absent"))));
    }

    #[test]
    fn first_program_context_growth_materializes_and_checks_new_original() {
        let interface = b"interface".to_vec();
        let value = Value::Array(vec![
            text("TPMOD"),
            Value::Integer(1.into()),
            Value::Array(vec![Value::Array(vec![
                text("fixture"),
                text("Support"),
                Value::Bytes(interface.clone()),
                Value::Array(vec![]),
            ])]),
        ]);
        let mut product_bytes = Vec::new();
        ciborium::ser::into_writer(&value, &mut product_bytes).unwrap();
        let owner = CachedHomeOwner {
            unit: "fixture".into(),
            module: "Support".into(),
            module_version: ModuleVersion([1; 32]),
            skinny_iface_sha256: Sha256::digest(&interface).into(),
            product_sha256: Sha256::digest(&product_bytes).into(),
        };
        let certification =
            crate::certified_products::encode_home_certification(&owner, &[], &BTreeMap::new())
                .unwrap();
        let package_value = Value::Array(vec![
            text("TPPKGROOTS"),
            text("2"),
            Value::Array(vec![
                text(&owner.unit),
                text(&owner.module),
                text(sha256(&interface)),
            ]),
            Value::Array(vec![]),
            Value::Array(vec![]),
        ]);
        let mut package_bytes = Vec::new();
        ciborium::ser::into_writer(&package_value, &mut package_bytes).unwrap();
        let product = crate::certified_products::fixture_finalized_product(
            CertifiedRecoveryProduct::from_certification(
                owner,
                interface,
                product_bytes,
                package_bytes,
                certification,
            ),
            [2; 32],
        );
        let baseline = Arc::new(ExactDeclarationContext::new(&[], &[], vec![]).unwrap());
        let context = Arc::new(
            baseline
                .as_ref()
                .clone()
                .extend_checked_original_products([2; 32], &[product])
                .unwrap(),
        );
        let directory = tempfile::tempdir().unwrap();
        let request = ExactCompilationRequest {
            context: baseline,
            manifest: directory.path().join("scope"),
            request_sha256: String::new(),
            semantic_sha256: [1; 32],
            producer_sha256: [2; 32],
            artifacts: vec![],
            groups: Arc::from([]),
            materialization: None,
            program_support: None,
            program_source_lexical: Vec::new(),
            source_selected_support: BTreeSet::new(),
            source_search_include: None,
            checked_value_imports: Default::default(),
            generated_scaffold_imports: Vec::new(),
        };
        let root = directory.path().join("program-inputs");
        assert!(!root.exists());
        let effective = request.in_program_context(&root, context).unwrap();
        assert_eq!(effective.artifacts.len(), 1);
        assert!(effective.artifacts[0].interface.path.is_file());
        effective
            .context
            .validate_artifacts(&effective.artifacts)
            .unwrap();
        std::fs::write(&effective.artifacts[0].interface.path, b"tampered").unwrap();
        assert!(effective
            .context
            .validate_artifacts(&effective.artifacts)
            .is_err());
    }
    #[test]
    fn program_context_promotes_canonical_owner_without_duplicate_materialization() {
        let product = support_product("Support");
        let unchanged = support_product("Unchanged");
        let inventory = ArtifactInventory::default();
        let interfaces = inventory
            .admit(
                &inventory.empty_view(),
                vec![
                    ArtifactEntry::canonical(product.module_interface().unwrap().clone()),
                    ArtifactEntry::canonical(unchanged.module_interface().unwrap().clone()),
                ],
            )
            .unwrap();
        let baseline = Arc::new(
            ExactDeclarationContext::from_authenticated_interfaces([2; 32], &interfaces).unwrap(),
        );
        let context = Arc::new(
            baseline
                .as_ref()
                .clone()
                .extend_checked_original_products([2; 32], &[product])
                .unwrap(),
        );
        assert_eq!(
            context.artifact_view().artifact_ids().len(),
            baseline.artifact_view().artifact_ids().len() + 1,
            "promotion retains the canonical carrier and adds its original product"
        );
        let directory = tempfile::tempdir().unwrap();
        let request = ExactCompilationRequest {
            artifacts: baseline.materialize_scratch(&directory).unwrap().artifacts,
            context: baseline,
            manifest: directory.path().join("scope"),
            request_sha256: String::new(),
            semantic_sha256: [1; 32],
            producer_sha256: [2; 32],
            groups: Arc::from([]),
            materialization: None,
            program_support: None,
            program_source_lexical: Vec::new(),
            source_selected_support: BTreeSet::new(),
            source_search_include: None,
            checked_value_imports: Default::default(),
            generated_scaffold_imports: Vec::new(),
        };
        let support = request
            .artifacts
            .iter()
            .position(|artifact| artifact.interface.module == "Support")
            .unwrap();
        let old_path = &request.artifacts[support].interface.path;
        let old_bytes = std::fs::read(old_path).unwrap();
        let root = directory.path().join("program-inputs");
        std::fs::write(old_path, b"tampered").unwrap();
        assert!(request.in_program_context(&root, context.clone()).is_err());
        assert!(
            !root.exists(),
            "displaced consumed bytes are checked before writing"
        );
        std::fs::write(old_path, old_bytes).unwrap();
        let mut duplicate = request.clone();
        duplicate.artifacts.push(request.artifacts[support].clone());
        assert!(duplicate
            .in_program_context(&root, context.clone())
            .is_err());
        let mut relative = request.clone();
        relative.artifacts[support].interface.path = PathBuf::from("relative.hi");
        assert!(relative.in_program_context(&root, context.clone()).is_err());

        let effective = request.in_program_context(&root, context.clone()).unwrap();
        assert_eq!(effective.artifacts.len(), 2);
        let promoted = effective
            .artifacts
            .iter()
            .find(|artifact| artifact.interface.module == "Support")
            .unwrap();
        assert!(promoted.product.is_some());
        assert_ne!(&promoted.interface.path, old_path);
        let unchanged_path = |artifacts: &[DeclarationArtifact]| {
            artifacts
                .iter()
                .find(|artifact| artifact.interface.module == "Unchanged")
                .unwrap()
                .interface
                .path
                .clone()
        };
        assert_eq!(
            unchanged_path(&effective.artifacts),
            unchanged_path(&request.artifacts)
        );
        context.validate_artifacts(&effective.artifacts).unwrap();
        let recomputed_directory = tempfile::tempdir().unwrap();
        let recomputed = context.materialize_scratch(&recomputed_directory).unwrap();
        let selections = |artifacts: &[DeclarationArtifact]| {
            artifacts
                .iter()
                .map(|artifact| {
                    (
                        identity(&artifact.interface.unit, &artifact.interface.module),
                        (
                            artifact.interface.sha256.clone(),
                            artifact.interface.requirements.clone(),
                            artifact
                                .product
                                .as_ref()
                                .map(|product| product.sha256.clone()),
                        ),
                    )
                })
                .collect::<BTreeMap<_, _>>()
        };
        assert_eq!(
            selections(&effective.artifacts),
            selections(&recomputed.artifacts)
        );
        let repeated_root = directory.path().join("unchanged-inputs");
        let repeated = effective
            .in_program_context(&repeated_root, context.clone())
            .unwrap();
        assert!(!repeated_root.exists());
        assert_eq!(repeated.artifacts.len(), 2);
        context.validate_artifacts(&repeated.artifacts).unwrap();
        std::fs::write(&promoted.product.as_ref().unwrap().path, b"tampered").unwrap();
        assert!(context.validate_artifacts(&repeated.artifacts).is_err());
    }

    #[test]
    fn unchanged_program_context_reuses_materialization_but_admission_checks_tampering() {
        let owner = CachedHomeOwner {
            unit: "fixture".into(),
            module: "Support".into(),
            module_version: ModuleVersion([1; 32]),
            skinny_iface_sha256: Sha256::digest(b"interface").into(),
            product_sha256: Sha256::digest(b"product").into(),
        };
        let certification =
            crate::certified_products::encode_home_certification(&owner, &[], &BTreeMap::new())
                .unwrap();
        let product = crate::certified_products::fixture_finalized_product(
            CertifiedRecoveryProduct::from_certification(
                owner,
                b"interface".to_vec(),
                b"product".to_vec(),
                Vec::new(),
                certification,
            ),
            [2; 32],
        );
        let context = Arc::new(
            ExactDeclarationContext::new(&[], &[], Vec::new())
                .unwrap()
                .extend_checked_original_products([2; 32], &[product])
                .unwrap(),
        );
        let directory = tempfile::tempdir().unwrap();
        let artifacts = vec![DeclarationArtifact {
            interface: ExactIfaceArtifact {
                unit: "fixture".into(),
                module: "Support".into(),
                path: directory.path().join("missing.hi"),
                sha256: sha256(b"interface"),
                requirements: Vec::new(),
            },
            product: Some(ModuleSnapshot {
                module: "Support".into(),
                path: directory.path().join("missing.bin"),
                sha256: sha256(b"product"),
            }),
        }];
        let request = ExactCompilationRequest {
            context: context.clone(),
            manifest: directory.path().join("scope"),
            request_sha256: String::new(),
            semantic_sha256: [1; 32],
            producer_sha256: [2; 32],
            artifacts,
            groups: Arc::from([]),
            materialization: None,
            program_support: None,
            program_source_lexical: Vec::new(),
            source_selected_support: BTreeSet::new(),
            source_search_include: None,
            checked_value_imports: Default::default(),
            generated_scaffold_imports: Vec::new(),
        };
        let materialization_root = directory.path().join("program-inputs");
        let effective = request
            .in_program_context(&materialization_root, context)
            .unwrap();
        assert!(!materialization_root.exists());
        assert_eq!(
            effective.artifacts[0].interface.path,
            request.artifacts[0].interface.path
        );
        assert!(Arc::ptr_eq(&effective.groups, &request.groups));
        assert!(effective
            .context
            .validate_artifacts(&effective.artifacts)
            .is_err());
    }

    #[test]
    fn supporting_originals_preserve_owned_products_without_lexical_exposure() {
        let owner = CachedHomeOwner {
            unit: "fixture".into(),
            module: "Support".into(),
            module_version: ModuleVersion([1; 32]),
            skinny_iface_sha256: Sha256::digest(b"interface").into(),
            product_sha256: Sha256::digest(b"product").into(),
        };
        let certification =
            crate::certified_products::encode_home_certification(&owner, &[], &BTreeMap::new())
                .unwrap();
        let product = crate::certified_products::fixture_finalized_product(
            CertifiedRecoveryProduct::from_certification(
                owner.clone(),
                b"interface".to_vec(),
                b"product".to_vec(),
                Vec::new(),
                certification.clone(),
            ),
            [2; 32],
        );
        let context = ExactDeclarationContext::new(&[], &[], Vec::new())
            .unwrap()
            .extend_checked_original_products([2; 32], std::slice::from_ref(&product))
            .unwrap();
        assert!(context.lexical_graph().is_empty());
        assert_eq!(context.artifact_view().descriptors().len(), 2);
        let repeated = context
            .clone()
            .extend_checked_original_products([2; 32], std::slice::from_ref(&product))
            .unwrap();
        assert_eq!(context.semantic_sha256(), repeated.semantic_sha256());
        assert_eq!(repeated.artifact_view().inventory().node_count(), 2);
        let changed = CertifiedRecoveryProduct::from_certification(
            owner,
            b"interface".to_vec(),
            b"changed".to_vec(),
            Vec::new(),
            certification,
        );
        assert!(context
            .clone()
            .extend_checked_original_products([2; 32], &[changed])
            .is_err());
        assert!(context
            .extend_checked_original_products([99; 32], &[product])
            .is_err());
    }

    fn execution_entry(
        owner: tidepool_repr::execution_schema::CachedHomeOwner,
        graph: Arc<crate::execution_source::CertifiedExecutionSourceGraph>,
    ) -> Arc<ArtifactEntry> {
        let mut validation = PackageInterfaceValidation::default();
        let seal =
            crate::certified_products::encode_home_certification(&owner, &[], &BTreeMap::new())
                .unwrap();
        let seal = crate::certified_products::bind_home_execution_source(
            &seal,
            &owner,
            graph.digest(),
            &mut validation,
        )
        .unwrap();
        let mut packages = Vec::new();
        ciborium::ser::into_writer(
            &Value::Array(vec![
                text("TPPKGROOTS"),
                text("2"),
                Value::Array(vec![
                    text(&owner.unit),
                    text(&owner.module),
                    text(hex(&owner.skinny_iface_sha256)),
                ]),
                Value::Array(vec![]),
                Value::Array(vec![]),
            ]),
            &mut packages,
        )
        .unwrap();
        let product = crate::certified_products::fixture_finalized_product(
            CertifiedRecoveryProduct::from_certification(
                owner,
                b"iface".to_vec(),
                b"product".to_vec(),
                packages,
                seal,
            ),
            [7; 32],
        )
        .with_execution_source_with_validation(graph, &mut validation)
        .unwrap();
        Arc::new(ArtifactEntry::original([7; 32], product).unwrap())
    }

    #[test]
    fn generated_scaffold_receipt_resolves_only_normalized_request_owner() {
        let directory = tempfile::tempdir().unwrap();
        let source = format!("module Consumer where\n{GENERATED_RESUME_IMPORT}\n");
        let context = Arc::new(
            ExactDeclarationContext::new(&[], &[], vec![])
                .unwrap()
                .extend_checked_original_products(
                    [2; 32],
                    &[support_product_in_unit("main", "Tidepool.Internal.Resume")],
                )
                .unwrap(),
        );
        let request = program_request(directory.path(), context.clone())
            .with_generated_scaffold_imports([source.as_str()]);
        assert!(context.lexical_graph().is_empty());
        let receipt = import_receipt_source_owner(
            directory.path(),
            &request,
            "main",
            "Tidepool.Internal.Resume",
            "none",
            false,
            &source,
        );
        let admitted = request.validate_receipt(&receipt, None, &context).unwrap();
        assert_eq!(
            admitted.evidence.modules[0].source,
            Path::new(crate::cache::GENERATED_SOURCE)
        );
        assert!(admitted
            .witness
            .matches_source(&directory.path().join("Consumer.hs"), &source));
        let mut unprotected = request.clone();
        unprotected.generated_scaffold_imports.clear();
        assert!(unprotected
            .validate_receipt(&receipt, None, &context)
            .is_err());

        // Identical bytes in a separate authored module do not grant the
        // compiler scaffold's request-target authority.
        let mut value: Value =
            ciborium::de::from_reader(std::fs::read(&receipt).unwrap().as_slice()).unwrap();
        let fields = value.as_array_mut().unwrap();
        let mut evidence: crate::cache::DependencyEvidence =
            serde_json::from_str(fields[7].as_text().unwrap()).unwrap();
        let authored = directory.path().join("Authored.hs");
        std::fs::write(&authored, &source).unwrap();
        evidence.sources.push(crate::cache::SourceEvidence {
            path: authored.clone(),
            sha256: sha256(source.as_bytes()),
        });
        evidence.modules[0].source = authored;
        fields[7] = text(serde_json::to_string(&evidence).unwrap());
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(&value, &mut bytes).unwrap();
        std::fs::write(&receipt, bytes).unwrap();
        assert!(request.validate_receipt(&receipt, None, &context).is_err());
    }

    pub(crate) fn assert_source_selected_receipt_pairing(
        root: &Path,
        module: tidepool_repr::SessionModule,
        original: &str,
        support: &str,
        request: ExactCompilationRequest,
        endpoint: tidepool_extract_cmd::CompilerEndpoint,
        offer: &crate::artifacts::ModuleCandidateOffer,
    ) {
        let original_path = root.join(module.relative_hs_path());
        let support_path = root.join("PrefixSelectedSupport.hs");
        let includes = vec![root.to_path_buf()];
        let owner = identity("main", "PrefixSelectedSupport");
        let persisted = Arc::clone(&request.context);
        let empty = ExactDeclarationContext::new(&[], &[], vec![]).unwrap();
        let output = root.join("consumer-output");
        std::fs::create_dir(&output).unwrap();
        let source_path = output.join("Consumer.hs");
        std::fs::write(&source_path,
            "module Consumer where\nimport PrefixSelectedSupport\nresult :: Int\nresult = selected\n__tidepool_inspect_0 = result\n").unwrap();
        let inspection_output = output.join("inspection.cbor");
        let mut command = tidepool_extract_cmd::ExtractCmd::new().unwrap();
        command
            .input(&source_path)
            .includes(&includes)
            .inspect_type("result")
            .inspect_out(&inspection_output)
            .inspection_strict()
            .output_dir(&output);
        offer.apply_to(&mut command).unwrap();
        let request_bytes = command.request_bytes();
        let decoded = tidepool_extract_cmd::ExtractRequest::decode(&request_bytes).unwrap();
        assert!(decoded.target_names().is_empty());
        assert!(decoded.retained_generations().is_empty());
        std::fs::write(root.join("worker-request.cbor"), request_bytes).unwrap();
        let run = endpoint.execute(&command).unwrap();
        std::fs::write(root.join("consumer.stdout"), &run.output.stdout).unwrap();
        std::fs::write(root.join("consumer.stderr"), &run.output.stderr).unwrap();
        crate::diag::decode_extract_result(run.success(), &run.output.stdout, &run.output.stderr)
            .expect("actual current-source inspection compiles");
        assert!(std::fs::metadata(&inspection_output).unwrap().len() > 0);
        let persisted_admissions = request.validate_outputs(&output).unwrap();
        assert_eq!(persisted_admissions.len(), 1);
        assert!(persisted_admissions[0]
            .selected_originals
            .contains_key(&owner));
        let receipts = std::fs::read_dir(output.join(".exact-compilations"))
            .unwrap()
            .map(|entry| entry.unwrap().path().join("receipt.cbor"))
            .collect::<Vec<_>>();
        assert_eq!(receipts.len(), 1);
        let receipt = &receipts[0];
        assert!(
            request.validate_receipt(receipt, None, &empty).is_err(),
            "current worker claims cannot supply absent prior custody"
        );
        let mut local = request.clone();
        local.program_support = Some(ProgramSourceSupport {
            artifacts: persisted.artifact_view().clone(),
            imports: Arc::new(BTreeMap::new()),
        });
        let local_admission = local.validate_receipt(receipt, None, &empty).unwrap();
        assert_eq!(
            local_admission.home_imports().unwrap(),
            persisted_admissions[0].home_imports().unwrap()
        );
        assert_eq!(
            local_admission.selected_originals.len(),
            persisted_admissions[0].selected_originals.len()
        );
        assert!(empty.artifact_view().is_empty());
        assert!(empty.lexical_graph().is_empty());
        let actual = read_receipt(receipt);
        let mut changed = actual.clone();
        changed.as_array_mut().unwrap()[9].as_array_mut().unwrap()[0]
            .as_array_mut()
            .unwrap()[0]
            .as_array_mut()
            .unwrap()[1] = text("UnadmittedOwner");
        write_receipt(receipt, &changed);
        assert!(local.validate_receipt(receipt, None, &empty).is_err());
        let mut changed = actual.clone();
        let selection = changed.as_array_mut().unwrap()[9].as_array_mut().unwrap();
        let mut evidence: crate::cache::DependencyEvidence =
            serde_json::from_str(selection[1].as_text().unwrap()).unwrap();
        evidence.modules[0]
            .imports
            .push(crate::cache::ModuleImportEvidence {
                qualifier: crate::cache::ImportQualifier::Unqualified,
                module: "UnadmittedOwner".into(),
                boot: false,
                selected: None,
            });
        selection[1] = text(serde_json::to_string(&evidence).unwrap());
        write_receipt(receipt, &changed);
        assert!(local.validate_receipt(receipt, None, &empty).is_err());
        write_receipt(receipt, &actual);
        std::fs::write(
            &support_path,
            support.replace("selected = 42", "selected = 43"),
        )
        .unwrap();
        assert!(local.validate_receipt(receipt, None, &empty).is_err());
        let conflicting_root = root.join("conflicting-issuer");
        std::fs::create_dir(&conflicting_root).unwrap();
        let changed = crate::declaration_join::certify_authored_declaration(
            module,
            &original_path,
            original,
            &includes,
            &conflicting_root,
        )
        .expect("actual compiler issues the changed original proof");
        std::fs::write(&support_path, support).unwrap();
        local.program_support = Some(ProgramSourceSupport {
            artifacts: changed
                .artifact_view()
                .interface_projection(&[owner])
                .unwrap(),
            imports: Arc::new(BTreeMap::new()),
        });
        assert!(
            local.validate_receipt(receipt, None, &persisted).is_err(),
            "local custody cannot override a conflicting persisted canonical owner"
        );
    }

    #[test]
    #[ignore = "requires the matched Haskell worker and frontend"]
    fn generated_planned_import_is_bound_to_original_certificate_and_target() {
        let directory = tempfile::tempdir().unwrap();
        let owner = tidepool_repr::SessionModule::lib(tidepool_repr::Generation(1));
        let original_source = include_str!("../tests/fixtures/checked-prefix-publication/G1.hs");
        let original_path = directory.path().join(owner.relative_hs_path());
        std::fs::create_dir_all(original_path.parent().unwrap()).unwrap();
        std::fs::write(&original_path, original_source).unwrap();
        std::fs::write(
            directory.path().join("PrefixSelectedSupport.hs"),
            include_str!("../tests/fixtures/checked-prefix-publication/PrefixSelectedSupport.hs"),
        )
        .unwrap();
        let certificate = Arc::new(
            crate::declaration_join::certify_authored_declaration(
                owner,
                &original_path,
                original_source,
                &[directory.path().to_path_buf()],
                directory.path(),
            )
            .expect("matched issuer supplies the original native and canonical carriers"),
        );
        let context = Arc::new(
            ExactDeclarationContext::new(std::slice::from_ref(&certificate), &[], Vec::new())
                .unwrap(),
        );
        assert!(context.lexical_graph().is_empty());
        let source = format!(
            "module Consumer where\n-- tidepool-preamble-imports-v1\nimport {}\n",
            owner.module_name()
        );
        let mut request = program_request(directory.path(), context.clone());
        request.producer_sha256 = certificate.toolchain_identity_sha256();
        let ordinary = request.clone();
        let request = request
            .with_generated_planned_imports(Some(&certificate), [source.as_str()])
            .unwrap();
        let receipt = import_receipt_source_owner(
            directory.path(),
            &request,
            "main",
            &owner.module_name(),
            "none",
            false,
            &source,
        );
        let admitted = request.validate_receipt(&receipt, None, &context).unwrap();
        assert_eq!(
            admitted.scaffold_roots.get(&0),
            Some(&BTreeSet::from([identity("main", &owner.module_name()),])),
            "original scope records only the actual protected recipe root"
        );
        let later_error = ordinary
            .validate_receipt(&receipt, None, &context)
            .err()
            .expect("ordinary input lacks protected original permission");
        assert!(matches!(later_error, CompileError::ExtractFailed(detail)
            if detail.contains("exact import witness leaves selected lexical graph: source fixture:Consumer")));
        let mut wrong_producer = ordinary.clone();
        wrong_producer.producer_sha256 = [0; 32];
        assert!(wrong_producer
            .with_generated_planned_imports(Some(&certificate), [source.as_str()])
            .is_err());
        let mut absent_original = ordinary;
        absent_original.context =
            Arc::new(ExactDeclarationContext::new(&[], &[], Vec::new()).unwrap());
        assert!(absent_original
            .with_generated_planned_imports(Some(&certificate), [source.as_str()])
            .is_err());

        // The protected target remains valid; a second source copying both
        // its marker and original import cannot borrow its edge permission.
        let mut value: Value =
            ciborium::de::from_reader(std::fs::read(&receipt).unwrap().as_slice()).unwrap();
        let fields = value.as_array_mut().unwrap();
        let mut evidence: crate::cache::DependencyEvidence =
            serde_json::from_str(fields[7].as_text().unwrap()).unwrap();
        let helper = directory.path().join("Helper.hs");
        let helper_source = source.replace("module Consumer where", "module Helper where");
        std::fs::write(&helper, &helper_source).unwrap();
        evidence.sources.push(crate::cache::SourceEvidence {
            path: helper.clone(),
            sha256: sha256(helper_source.as_bytes()),
        });
        evidence.modules.push(crate::cache::ModuleEvidence {
            unit: "fixture".into(),
            module: "Helper".into(),
            boot: false,
            source: helper,
            imports: vec![],
            product: crate::cache::ProductAvailability::Ready,
        });
        fields[7] = text(serde_json::to_string(&evidence).unwrap());
        fields[8].as_array_mut().unwrap().push(Value::Array(vec![
            text("fixture"),
            text("Helper"),
            Value::Bool(false),
            Value::Array(vec![Value::Array(vec![
                text("none"),
                text(owner.module_name()),
                Value::Bool(false),
                text("main"),
            ])]),
        ]));
        write_receipt(&receipt, &value);
        let helper_error = request
            .validate_receipt(&receipt, None, &context)
            .err()
            .expect("helper cannot borrow the protected target edge");
        assert!(matches!(helper_error, CompileError::ExtractFailed(detail)
            if detail.contains("exact import witness leaves selected lexical graph: source fixture:Helper")));
    }

    #[test]
    fn checked_template_graph_retains_lexical_dependencies_without_artifact_promotion() {
        let (initial, _) = metadata_fixture();
        let template = "module Expr where\nimport Joined hiding (joinedValue)\n";
        let graph = initial
            .template_interface_graph(&[template.to_owned()])
            .unwrap();
        assert_eq!(graph.len(), 2);
        assert_eq!(
            graph[&identity("fixture", "Joined")].imports,
            vec![identity("fixture", "Alpha")]
        );
        assert!(graph.contains_key(&identity("fixture", "Alpha")));
        assert!(
            !graph.contains_key(&identity("fixture", "Beta")),
            "artifact requirements cannot grant lexical instances"
        );
        assert!(
            !template_selects_owner(&[template.to_owned()], &identity("fixture", "JoinedExtra")),
            "an import does not select a longer module name"
        );
        assert!(
            !template_selects_owner(
                &["import JoinedExtra hiding (x)".to_owned()],
                &identity("fixture", "Joined")
            ),
            "a longer imported module name does not select a prefix owner"
        );
        assert!(
            template_selects_owner(
                &["import qualified Joined as J hiding (x)".to_owned()],
                &identity("fixture", "Joined")
            ),
            "qualified imports with aliases and modifiers retain their exact owner"
        );
        let mut current = initial.as_ref().clone();
        current.lexical.clear();
        let authority = GeneratedScaffoldImportAuthority {
            role: GeneratedScaffoldRole::InitialTemplateInterfaces {
                producer: initial.producer,
                roots: BTreeSet::from([identity("fixture", "Joined")]),
                graph: Arc::new(graph.clone()),
            },
            protected_templates: Arc::from([template.to_owned()]),
        };
        assert_eq!(
            authority
                .original_instance_graph()
                .iter()
                .map(|node| node.owner.clone())
                .collect::<BTreeSet<_>>(),
            graph.keys().cloned().collect(),
            "protected instance scope retains the graph issued at template admission"
        );
        let target = Path::new("/owned/Expr.hs");
        assert!(authority
            .permits(&current, template, target, target, "fixture", "Joined", "none", false));
        assert!(
            !authority.permits(
                &current,
                &format!("{template}import Alpha\n"),
                target,
                target,
                "fixture",
                "Alpha",
                "none",
                false
            ),
            "retained lexical dependencies are not independent target imports"
        );
    }

    #[test]
    fn checked_template_interfaces_preserve_seals_without_native_or_lexical_authority() {
        let make_context = |bytes: &[u8], selected: bool| {
            let owner = identity("main", "CapturedInterface");
            let mut packages = Vec::new();
            ciborium::ser::into_writer(
                &Value::Array(vec![
                    text("TPPKGROOTS"),
                    text("2"),
                    Value::Array(vec![
                        text(&owner.unit),
                        text(&owner.module),
                        text(sha256(bytes)),
                    ]),
                    Value::Array(vec![]),
                    Value::Array(vec![]),
                ]),
                &mut packages,
            )
            .unwrap();
            let interface = CertifiedJoinedInterface::from_certification(
                [7; 32],
                owner.unit.clone(),
                owner.module.clone(),
                bytes.to_vec(),
                packages,
            )
            .unwrap();
            let inventory = ArtifactInventory::default();
            Arc::new(ExactDeclarationContext {
                producer: [7; 32],
                original_instance_environment: OriginalInstanceEnvironment::Unknown,
                template_imports: None,
                inventory: inventory
                    .admit(
                        &inventory.empty_view(),
                        vec![ArtifactEntry::interface(
                            interface,
                            JoinedInterfaceRole::LexicalJoin,
                            vec![],
                        )],
                    )
                    .unwrap(),
                lexical: if selected {
                    vec![ExactLexicalNode {
                        owner,
                        imports: vec![],
                    }]
                } else {
                    vec![]
                },
            })
        };
        let initial = make_context(b"first interface", true);
        let current = make_context(b"first interface", false);
        let changed = make_context(b"changed interface", false);
        assert!(initial.original_preview_interface_graph().is_err());
        let joined_target = ExactDeclarationContext::from_authenticated_execution(
            initial.producer,
            initial.artifact_view(),
            initial.lexical_graph().to_vec(),
            identity("main", "CapturedInterface"),
            &[identity("main", "CapturedInterface")],
        )
        .unwrap();
        assert!(
            joined_target.original_instance_target().is_err(),
            "a lexical join cannot replace the original canonical target's import census"
        );
        assert!(joined_target.original_preview_interface_graph().is_err());
        let canonical_target =
            ArtifactEntry::canonical(crate::certified_products::fixture_module_interface(
                initial.producer,
                "main",
                "OriginalTarget",
                BTreeMap::new(),
            ));
        let inventory = initial.inventory.inventory();
        let retained = inventory
            .admit(initial.artifact_view(), vec![canonical_target])
            .unwrap();
        let mut lexical = initial.lexical_graph().to_vec();
        lexical.push(ExactLexicalNode {
            owner: identity("main", "OriginalTarget"),
            imports: vec![],
        });
        let original = ExactDeclarationContext::from_authenticated_execution(
            initial.producer,
            &retained,
            lexical,
            identity("main", "OriginalTarget"),
            &[identity("main", "CapturedInterface")],
        )
        .unwrap();
        assert_eq!(
            original.original_instance_target().unwrap(),
            &identity("main", "OriginalTarget")
        );
        let mut exchanged = original.clone();
        exchanged.original_instance_environment = OriginalInstanceEnvironment::Complete {
            target: identity("main", "OtherTarget"),
        };
        assert_ne!(
            original.semantic_sha256(),
            exchanged.semantic_sha256(),
            "the context commitment binds the original target identity"
        );
        assert!(
            exchanged.original_preview_interface_graph().is_err(),
            "a retained graph cannot replace its missing original target"
        );
        let original_graph = original.original_preview_interface_graph().unwrap();
        assert_eq!(original_graph.len(), 2);
        assert!(original
            .template_interface_graph(&["module Input where\n".to_owned()])
            .unwrap()
            .is_empty());
        let mut malformed = original.clone();
        malformed.lexical[0]
            .imports
            .push(identity("main", "MissingOriginal"));
        assert!(malformed.original_preview_interface_graph().is_err());
        let mut foreign = original;
        foreign.lexical[0].owner.unit = "foreign".into();
        assert!(foreign.original_preview_interface_graph().is_err());
        let source = "module Expr where\nimport CapturedInterface\n";
        let authority = GeneratedScaffoldImportAuthority {
            role: GeneratedScaffoldRole::InitialTemplateInterfaces {
                producer: initial.producer,
                roots: BTreeSet::from([identity("main", "CapturedInterface")]),
                graph: Arc::new(
                    initial
                        .template_interface_graph(&[source.to_owned()])
                        .unwrap(),
                ),
            },
            protected_templates: Arc::from([source.to_owned()]),
        };
        let target = Path::new("/owned/Expr.hs");
        let permits = |context: &ExactDeclarationContext, source: &str, module_source: &Path| {
            authority.permits(
                context,
                source,
                target,
                module_source,
                "main",
                "CapturedInterface",
                "none",
                false,
            )
        };
        assert!(permits(&current, source, target));
        assert!(current.recovery_products().is_empty());
        assert!(current.lexical_graph().is_empty());
        assert!(!permits(&changed, source, target));
        assert!(!permits(&current, source, Path::new("/owned/Authored.hs")));
        assert!(!permits(
            &current,
            "module Expr where\nimport qualified CapturedInterface as Hidden\n",
            target
        ));
        assert!(!permits(
            &current,
            &format!("{source}import CapturedInterface\n"),
            target
        ));
        let repeated_source = "module Expr where\n\
            import CapturedInterface hiding (hidden)\n\
            import qualified CapturedInterface as Captured\n\
            import qualified CapturedInterface as Captured\n";
        let repeated = GeneratedScaffoldImportAuthority {
            protected_templates: Arc::from([repeated_source.to_owned()]),
            ..authority.clone()
        };
        let repeated_permits = |source: &str| {
            repeated.permits(
                &current,
                source,
                target,
                target,
                "main",
                "CapturedInterface",
                "none",
                false,
            )
        };
        assert!(
            repeated_permits(repeated_source),
            "each protected alias and repeated occurrence retains its authority"
        );
        assert!(
            !repeated_permits(&repeated_source.replacen(
                "import qualified CapturedInterface as Captured\n",
                "",
                1,
            )),
            "a missing protected occurrence cannot be hidden by owner equality"
        );
        assert!(
            !repeated_permits(&repeated_source.replacen("as Captured", "as Changed", 1,)),
            "a changed protected alias needs new source authority"
        );
        assert!(
            !repeated_permits(&format!(
                "{repeated_source}import qualified CapturedInterface as Captured\n"
            )),
            "an extra authored occurrence needs ordinary source proof"
        );
        assert!(
            !repeated_permits(&repeated_source.replace("hiding (hidden)", "(hidden)",)),
            "changing hiding to an explicit import cannot preserve the protected grant"
        );
        let no_initial_selection = GeneratedScaffoldImportAuthority {
            role: GeneratedScaffoldRole::InitialTemplateInterfaces {
                producer: current.producer,
                roots: BTreeSet::new(),
                graph: Arc::new(
                    current
                        .template_interface_graph(&[source.to_owned()])
                        .unwrap(),
                ),
            },
            protected_templates: Arc::from([source.to_owned()]),
        };
        assert!(!no_initial_selection.permits(
            &current,
            source,
            target,
            target,
            "main",
            "CapturedInterface",
            "none",
            false
        ));
        let no_protected_import = GeneratedScaffoldImportAuthority {
            role: GeneratedScaffoldRole::InitialTemplateInterfaces {
                producer: initial.producer,
                roots: BTreeSet::new(),
                graph: Arc::new(
                    initial
                        .template_interface_graph(&["module Expr where\n".to_owned()])
                        .unwrap(),
                ),
            },
            protected_templates: Arc::from(["module Expr where\n".to_owned()]),
        };
        assert!(!no_protected_import.permits(
            &current,
            source,
            target,
            target,
            "main",
            "CapturedInterface",
            "none",
            false
        ));
        assert_eq!(initial.lexical_graph().len(), 1);
        let directory = tempfile::tempdir().unwrap();
        let empty = Arc::new(ExactDeclarationContext::new(&[], &[], vec![]).unwrap());
        let request = empty
            .prepare_compilation_authorizing(directory.path(), b"new compiler producer", |_| {
                Ok(Value::Null)
            })
            .unwrap();
        let unchanged = request
            .clone()
            .with_initial_template_interfaces(empty, &[source.to_owned()])
            .unwrap();
        assert!(unchanged.generated_scaffold_imports.is_empty());
        let wrong_producer =
            request.with_initial_template_interfaces(initial, &[source.to_owned()]);
        assert!(
            matches!(wrong_producer, Err(CompileError::ExtractFailed(reason))
            if reason == "exact declaration context: checked template interface producer differs")
        );
    }

    #[test]
    fn generated_scaffold_authority_preserves_target_and_original_owner() {
        let directory = tempfile::tempdir().unwrap();
        let (graph, owners) = crate::execution_source::test_graph(directory.path());
        let inventory = ArtifactInventory::default();
        let context = ExactDeclarationContext {
            producer: [7; 32],
            original_instance_environment: OriginalInstanceEnvironment::Unknown,
            template_imports: None,
            inventory: inventory
                .admit_shared(
                    &inventory.empty_view(),
                    vec![execution_entry(owners[0].clone(), graph)],
                )
                .unwrap(),
            lexical: vec![],
        };
        let authority = GeneratedScaffoldImportAuthority {
            role: GeneratedScaffoldRole::Native(NativeScaffoldRole::Resume(owners[0].clone())),
            protected_templates: Arc::from([format!(
                "module Expr where\n{GENERATED_RESUME_IMPORT}\n"
            )]),
        };
        let target = directory.path().join("Expr.hs");
        let source = format!("module Expr where\n{GENERATED_RESUME_IMPORT}\n");
        let permits = |source: &str,
                       module_source: &Path,
                       unit: &str,
                       module: &str,
                       qualifier: &str,
                       boot| {
            authority.permits(
                &context,
                source,
                &target,
                module_source,
                unit,
                module,
                qualifier,
                boot,
            )
        };
        assert!(permits(
            &source,
            &target,
            &owners[0].unit,
            &owners[0].module,
            "none",
            false
        ));
        assert!(context.lexical_graph().is_empty());
        assert!(!permits(
            &source,
            &directory.path().join("Authored.hs"),
            &owners[0].unit,
            &owners[0].module,
            "none",
            false
        ));
        for (unit, module, qualifier, boot) in [
            ("foreign", owners[0].module.as_str(), "none", false),
            (
                owners[0].unit.as_str(),
                owners[1].module.as_str(),
                "none",
                false,
            ),
            (
                owners[0].unit.as_str(),
                owners[0].module.as_str(),
                "this:fixture",
                false,
            ),
            (
                owners[0].unit.as_str(),
                owners[0].module.as_str(),
                "none",
                true,
            ),
        ] {
            assert!(!permits(&source, &target, unit, module, qualifier, boot));
        }
        assert!(permits(
            &format!("{{-# LANGUAGE GADTs #-}}\n{source}"),
            &target,
            &owners[0].unit,
            &owners[0].module,
            "none",
            false
        ));
        for changed in [
            format!("{source}{GENERATED_RESUME_IMPORT}\n"),
            source.replace(" as TidepoolResume", " as Other"),
        ] {
            assert!(!permits(
                &changed,
                &target,
                &owners[0].unit,
                &owners[0].module,
                "none",
                false
            ));
        }
        let mut changed = authority.clone();
        let GeneratedScaffoldRole::Native(NativeScaffoldRole::Resume(owner)) = &mut changed.role
        else {
            panic!("resume scaffold authority must retain its closed role");
        };
        owner.module_version = ModuleVersion([99; 32]);
        assert!(!changed.permits(
            &context,
            &source,
            &target,
            &target,
            &owners[0].unit,
            &owners[0].module,
            "none",
            false
        ));
    }

    fn execution_scope_fixture(
        entries: &[Arc<ArtifactEntry>],
        parent: &Path,
    ) -> Result<Option<Value>, CompileError> {
        let root = tempfile::tempdir_in(parent)?.keep();
        execution_scope_value(entries, &root)
    }

    #[test]
    fn execution_scope_borrows_parent_owned_graph_files_without_payload_writes() {
        let source = tempfile::tempdir().unwrap();
        let (graph, owners) = crate::execution_source::test_graph(source.path());
        let entries = [
            execution_entry(owners[0].clone(), Arc::clone(&graph)),
            execution_entry(owners[1].clone(), Arc::clone(&graph)),
        ];
        let parent = tempfile::tempdir().unwrap();
        let child = tempfile::tempdir().unwrap();
        let mut files = BTreeMap::new();
        let mut parent_written_bytes = 0;
        let first = execution_scope_value_with_graph_paths(
            &entries,
            parent.path(),
            &mut files,
            &mut parent_written_bytes,
        )
        .unwrap();
        assert_eq!(parent_written_bytes, graph.bytes().len() as u64);
        assert_eq!(files.len(), 1);
        let mut child_written_bytes = 0;
        let second = execution_scope_value_with_graph_paths(
            &entries,
            child.path(),
            &mut files,
            &mut child_written_bytes,
        )
        .unwrap();
        assert_eq!(
            first, second,
            "the child selects its parent's exact descriptor"
        );
        assert_eq!(child_written_bytes, 0);
        assert_eq!(std::fs::read_dir(child.path()).unwrap().count(), 0);
        assert!(files[&graph.digest()].path.starts_with(parent.path()));
        assert_eq!(
            std::fs::read(&files[&graph.digest()].path).unwrap(),
            graph.bytes()
        );
        println!("retained-graph parent_written_bytes={parent_written_bytes} descendant_written_bytes={child_written_bytes} graph_files=1");
    }

    #[test]
    fn execution_scope_shares_graph_and_matches_selected_original_closure() {
        let source = tempfile::tempdir().unwrap();
        let (graph, owners) = crate::execution_source::test_graph(source.path());
        let a = execution_entry(owners[0].clone(), graph.clone());
        let b = execution_entry(owners[1].clone(), graph.clone());
        let scope = execution_scope_fixture(&[a, b.clone()], source.path())
            .unwrap()
            .unwrap();
        let rows = scope.as_array().unwrap();
        assert_eq!(rows[0].as_array().unwrap().len(), 1);
        assert_eq!(rows[1].as_array().unwrap().len(), 2);
        let graph_a = crate::execution_source::test_graph_requiring_original(
            &graph,
            &owners[1],
            graph.digest(),
        );
        let a = execution_entry(owners[0].clone(), graph_a);
        let scope = execution_scope_fixture(&[a.clone(), b.clone()], source.path())
            .unwrap()
            .unwrap();
        assert_eq!(scope.as_array().unwrap()[0].as_array().unwrap().len(), 2);
        assert_eq!(scope.as_array().unwrap()[1].as_array().unwrap().len(), 2);
        let scope = execution_scope_fixture(&[a], source.path())
            .unwrap()
            .unwrap();
        assert!(
            scope.as_array().unwrap()[1].as_array().unwrap().is_empty(),
            "a graph digest cannot discover a missing original owner"
        );
        let mut wrong = owners[1].clone();
        wrong.module_version = tidepool_repr::execution_schema::ModuleVersion([99; 32]);
        let graph_a =
            crate::execution_source::test_graph_requiring_original(&graph, &wrong, graph.digest());
        let scope = execution_scope_fixture(
            &[execution_entry(owners[0].clone(), graph_a), b],
            source.path(),
        )
        .unwrap()
        .unwrap();
        let roots = scope.as_array().unwrap()[1].as_array().unwrap();
        assert_eq!(
            roots.len(),
            1,
            "wrong original version refuses dependent execution only"
        );
        assert_eq!(roots[0].as_array().unwrap()[1], text("B"));
        let large =
            crate::execution_source::test_graph_with_large_origin(&graph, EXACT_SCOPE_BYTES_LIMIT);
        assert!(execution_scope_fixture(
            &[execution_entry(owners[0].clone(), large)],
            source.path()
        )
        .unwrap()
        .is_some());
        let megabyte = crate::execution_source::test_graph_with_large_origin(&graph, 1 << 20);
        assert!(
            validate_execution_graph_budget(std::iter::repeat_n(megabyte.as_ref(), 64)).is_err(),
            "graph budget overflow explicitly refuses preparation before transport copies"
        );
        assert!(
            validate_execution_graph_budget(std::iter::repeat_n(
                graph.as_ref(),
                EXACT_SCOPE_GRAPHS_LIMIT + 1
            ))
            .is_err(),
            "graph count is bounded before transport allocation"
        );
    }

    #[test]
    fn execution_scope_preserves_complete_retained_closure_above_four_mib() {
        let temporary = tempfile::tempdir().unwrap();
        let root = std::env::var_os("TIDEPOOL_EXECUTION_SOURCE_FIXTURE_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| temporary.path().to_path_buf())
            .join("large-closure");
        std::fs::create_dir_all(&root).unwrap();
        let (graph, owners) = crate::execution_source::test_graph(&root);
        let retained = crate::execution_source::test_graph_with_large_origin(&graph, 2_300_000);
        let graph = crate::execution_source::test_graph_requiring_original(
            &retained,
            &owners[1],
            retained.digest(),
        );
        assert!(graph.bytes().len() + retained.bytes().len() > EXACT_SCOPE_BYTES_LIMIT);
        let entries = [
            execution_entry(owners[0].clone(), graph),
            execution_entry(owners[1].clone(), retained),
        ];
        let execution = execution_scope_value(&entries, &root).unwrap().unwrap();
        assert_eq!(
            execution.as_array().unwrap()[0].as_array().unwrap().len(),
            2
        );
        assert_eq!(
            execution.as_array().unwrap()[1].as_array().unwrap().len(),
            2
        );
        let interfaces = owners[..2]
            .iter()
            .map(|owner| {
                Value::Array(vec![
                    text(&owner.unit),
                    text(&owner.module),
                    path_value(&root.join(format!("{}.hi", owner.module))).unwrap(),
                    text(hex(&owner.skinny_iface_sha256)),
                    Value::Array(vec![]),
                    path_value(&root.join(format!("{}.packages", owner.module))).unwrap(),
                    text(hex(&[0; 32])),
                ])
            })
            .collect();
        let products = owners[..2]
            .iter()
            .map(|owner| {
                Value::Array(vec![
                    text(&owner.unit),
                    text(&owner.module),
                    text(hex(&owner.module_version.0)),
                    text(hex(&owner.skinny_iface_sha256)),
                    text(hex(&owner.product_sha256)),
                    path_value(&root.join(format!("{}.tpmod", owner.module))).unwrap(),
                    Value::Array(vec![]),
                ])
            })
            .collect();
        let fields = vec![
            text("TPEXACTSCOPE"),
            text("2"),
            text(hex(&[8; 32])),
            text(hex(&[7; 32])),
            Value::Array(interfaces),
            Value::Array(vec![]),
            Value::Array(products),
        ];
        let manifest = encode_scope_manifest(fields, Some(execution), None).unwrap();
        let decoded: Value = ciborium::de::from_reader(manifest.as_slice()).unwrap();
        assert_eq!(decoded.as_array().unwrap()[1], text("9"));
        std::fs::write(root.join("exact-declaration-scope.cbor"), manifest).unwrap();
        let missing = execution_scope_fixture(&entries[..1], &root)
            .unwrap()
            .unwrap();
        assert!(missing.as_array().unwrap()[1]
            .as_array()
            .unwrap()
            .is_empty());
        println!("scope6 complete retained closure: two graphs above four MiB retained; missing original refused");
    }

    #[test]
    fn execution_scope_retains_recipe_custody_across_local_owner_drift() {
        let source = tempfile::tempdir().unwrap();
        let (graph, owners) =
            crate::execution_source::test_graph_with_local_source_dependency(source.path());
        let a = execution_entry(owners[0].clone(), Arc::clone(&graph));
        let native_entry = |owner: CachedHomeOwner| {
            let seal =
                crate::certified_products::encode_home_certification(&owner, &[], &BTreeMap::new())
                    .unwrap();
            let mut packages = Vec::new();
            ciborium::ser::into_writer(
                &(
                    "TPPKGROOTS",
                    "2",
                    (&owner.unit, &owner.module, hex(&owner.skinny_iface_sha256)),
                    Vec::<Value>::new(),
                    Vec::<Value>::new(),
                ),
                &mut packages,
            )
            .unwrap();
            let product = crate::certified_products::fixture_finalized_product(
                CertifiedRecoveryProduct::from_certification(
                    owner,
                    b"iface".to_vec(),
                    b"product".to_vec(),
                    packages,
                    seal,
                ),
                graph.producer_sha256(),
            );
            Arc::new(ArtifactEntry::original(graph.producer_sha256(), product).unwrap())
        };
        let b1 = native_entry(owners[1].clone());
        let scope = execution_scope_fixture(&[Arc::clone(&a), Arc::clone(&b1)], source.path())
            .unwrap()
            .unwrap();
        assert_eq!(scope.as_array().unwrap()[1].as_array().unwrap().len(), 1);
        let mut changed = owners[1].clone();
        changed.module_version = ModuleVersion([99; 32]);
        let scope =
            execution_scope_fixture(&[Arc::clone(&a), native_entry(changed)], source.path())
                .unwrap()
                .unwrap();
        assert!(scope.as_array().unwrap()[1].as_array().unwrap().is_empty());
        assert!(
            scope.as_array().unwrap()[0].as_array().unwrap().is_empty(),
            "unadmitted graphs stay in custody, outside the execution projection"
        );
        let ArtifactPayload::Original(original) = &a.payload else {
            unreachable!()
        };
        assert!(Arc::ptr_eq(original.execution_source().unwrap(), &graph));
        let scope = execution_scope_fixture(&[a, b1], source.path())
            .unwrap()
            .unwrap();
        assert_eq!(scope.as_array().unwrap()[1].as_array().unwrap().len(), 1);
    }

    #[test]
    fn execution_manifest_separates_graph_budget_and_never_erases_authority() {
        use crate::cache::{
            DependencyEvidence, ModuleEvidence, ProductAvailability, SourceEvidence,
        };
        use crate::execution_source::{
            CertifiedExecutionSourceGraph, ExecutionSourceAdmission, ExecutionSourceGraphInput,
        };

        let temporary = tempfile::tempdir().unwrap();
        let root = std::env::var_os("TIDEPOOL_EXECUTION_SOURCE_FIXTURE_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| temporary.path().to_path_buf());
        std::fs::create_dir_all(&root).unwrap();
        let source_root = root.join("source");
        std::fs::create_dir_all(&source_root).unwrap();
        let generated = format!("module Target where\n--{}\n", "x".repeat(2300000));
        let authored = "module A where\n";
        let target_path = source_root.join("Target.hs");
        let source_path = source_root.join("A.hs");
        std::fs::write(&target_path, &generated).unwrap();
        std::fs::write(&source_path, authored).unwrap();
        let product = support_product("A");
        let owner = product.owner().clone();
        let producer = b"execution manifest producer";
        let producer_sha256 = Sha256::digest(producer).into();
        let product =
            crate::certified_products::fixture_finalized_product(product, producer_sha256);
        let evidence = DependencyEvidence {
            version: 4,
            cache_safe: true,
            selection_complete: true,
            sources: vec![
                SourceEvidence {
                    path: "@generated-source".into(),
                    sha256: sha256(generated.as_bytes()),
                },
                SourceEvidence {
                    path: source_path.clone(),
                    sha256: sha256(authored.as_bytes()),
                },
            ],
            resolutions: vec![],
            packages: vec![],
            modules: vec![ModuleEvidence {
                unit: owner.unit.clone(),
                module: owner.module.clone(),
                boot: false,
                source: source_path,
                imports: vec![],
                product: ProductAvailability::Ready,
            }],
        };
        let graph = CertifiedExecutionSourceGraph::admit(ExecutionSourceGraphInput {
            producer: crate::artifact_inventory::CanonicalProducerIdentity::from_producer_bytes(
                producer,
            ),
            semantic_sha256: None,
            include: &[source_root],
            source_path: &target_path,
            source: &generated,
            evidence: &evidence,
            owners: std::slice::from_ref(&owner),
            fresh_owners: &BTreeSet::from([identity(&owner.unit, &owner.module)]),
            retained_sources: &BTreeMap::new(),
            exact_imports: &BTreeMap::new(),
            packages: &BTreeMap::new(),
        })
        .unwrap();
        let ExecutionSourceAdmission::Available(graph) = graph else {
            panic!("authored original recipe unavailable")
        };
        let mut validation = PackageInterfaceValidation::default();
        let seal = crate::certified_products::bind_home_execution_source(
            product.certification_bytes(),
            &owner,
            graph.digest(),
            &mut validation,
        )
        .unwrap();
        let product = CertifiedRecoveryProduct::from_certification(
            owner,
            product.interface_bytes().to_vec(),
            product.product_bytes().to_vec(),
            product.package_imports_bytes().to_vec(),
            seal,
        )
        .with_module_interface(product.module_interface().unwrap().clone())
        .unwrap()
        .with_execution_source_with_validation(graph.clone(), &mut validation)
        .unwrap();
        let context = Arc::new(
            ExactDeclarationContext::new(&[], &[], vec![])
                .unwrap()
                .extend_checked_original_products(producer_sha256, &[product])
                .unwrap(),
        );
        let collision = root.join("collision");
        std::fs::create_dir_all(&collision).unwrap();
        let collision_graph = collision.join(format!("execution-{}.cbor", hex(&graph.digest())));
        std::fs::write(&collision_graph, b"prior immutable capture").unwrap();
        let collision_request = context.prepare_compilation(&collision, producer).unwrap();
        assert!(collision_request.materialization.is_some());
        assert_eq!(
            std::fs::read(&collision_graph).unwrap(),
            b"prior immutable capture",
            "graph capture cannot replace or truncate an existing request artifact"
        );
        let request = context
            .prepare_compilation(&root.join("ordinary"), producer)
            .unwrap();
        let bytes = std::fs::read(&request.manifest).unwrap();
        let value: Value = ciborium::de::from_reader(bytes.as_slice()).unwrap();
        let fields = value.as_array().unwrap();
        assert_eq!(fields.len(), 9);
        assert_eq!(fields[1], text("9"));
        assert_eq!(fields[8], Value::Null);
        assert_eq!(
            fields[7].as_array().unwrap()[1].as_array().unwrap().len(),
            1
        );
        std::fs::write(root.join("execution-source.cbor"), graph.bytes()).unwrap();
        let graph_row = &fields[7].as_array().unwrap()[0].as_array().unwrap()[0];
        let graph_path = graph_row.as_array().unwrap()[1].as_text().unwrap();
        assert_eq!(std::fs::read(graph_path).unwrap(), graph.bytes());
        assert!(context
            .prepare_compilation(&root.join("ordinary"), producer)
            .is_err());
        assert_eq!(
            std::fs::read(graph_path).unwrap(),
            graph.bytes(),
            "a reused request directory cannot truncate an already captured graph"
        );
        let mut inflated = fields[..7].to_vec();
        let interfaces = inflated[4].as_array_mut().unwrap();
        for index in 0..4000 {
            interfaces.push(Value::Array(vec![
                text("fixture"),
                text(format!("Padding{index}{}", "X".repeat(550))),
                text(root.join(format!("padding-{index}.hi")).to_str().unwrap()),
                text(hex(&[0; 32])),
                Value::Array(vec![]),
                text(
                    root.join(format!("padding-{index}.packages"))
                        .to_str()
                        .unwrap(),
                ),
                text(hex(&[0; 32])),
            ]));
        }
        let inflated_bytes =
            encode_scope_manifest(inflated, Some(fields[7].clone()), None).unwrap();
        assert!(graph.bytes().len() < EXACT_SCOPE_BYTES_LIMIT);
        assert!(inflated_bytes.len() < EXACT_SCOPE_BYTES_LIMIT);
        assert!(graph.bytes().len() + inflated_bytes.len() > EXACT_SCOPE_BYTES_LIMIT);
        let inflated_value: Value = ciborium::de::from_reader(inflated_bytes.as_slice()).unwrap();
        assert_eq!(inflated_value.as_array().unwrap()[7], fields[7]);
        std::fs::write(root.join("ordinary/budget-scope.cbor"), &inflated_bytes).unwrap();
        println!(
            "scope6 independent budgets: metadata={} graph={} combined={}",
            inflated_bytes.len(),
            graph.bytes().len(),
            inflated_bytes.len() + graph.bytes().len()
        );

        let base = fields[..7].to_vec();
        let authorization = Value::Array(vec![
            text("authorized"),
            text(hex(&request.semantic_sha256)),
        ]);
        for authorization in [None, Some(authorization)] {
            let mut expected = base.clone();
            expected[1] = text("9");
            expected.push(Value::Null);
            expected.push(authorization.clone().unwrap_or(Value::Null));
            let mut legacy = Vec::new();
            ciborium::ser::into_writer(&Value::Array(expected), &mut legacy).unwrap();
            assert_eq!(
                encode_scope_manifest(base.clone(), None, authorization.clone()).unwrap(),
                legacy,
                "v9 always retains explicit execution and purpose fields"
            );
            let overflow = Value::Array(vec![
                Value::Bytes(vec![0; EXACT_SCOPE_BYTES_LIMIT - 32]),
                Value::Array(vec![]),
            ]);
            assert!(
                encode_scope_manifest(base.clone(), Some(overflow), authorization.clone()).is_err(),
                "metadata overflow must never erase admitted execution authority"
            );
            let with_recipe =
                encode_scope_manifest(base.clone(), Some(fields[7].clone()), authorization.clone())
                    .unwrap();
            let decoded: Value = ciborium::de::from_reader(with_recipe.as_slice()).unwrap();
            let decoded = decoded.as_array().unwrap();
            assert_eq!(decoded[1], text("9"));
            assert_eq!(decoded[8], authorization.unwrap_or(Value::Null));
        }
    }
}
