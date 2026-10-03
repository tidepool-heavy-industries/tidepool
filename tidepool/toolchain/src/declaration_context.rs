//! Exact declaration inputs retain original products independently of source
//! lookup, while their explicit virtual graph owns lexical visibility.

use ciborium::value::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::artifact_inventory::{
    ArtifactEntry, ArtifactInventory, ArtifactKind, ArtifactMetadataSnapshot, ArtifactPayload,
    ArtifactView,
};
use crate::certified_products::{
    certify_owned_products_in_context_with_validation, PendingCertifiedGroup,
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

/// Request-local compiler inputs. Persistent publication retains declarations
/// separately, so native request annotations cannot enter a lexical snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExactCompileContext {
    declarations: Arc<ExactDeclarationContext>,
    request_types: Option<Arc<crate::checked_cell::RequestTypeSignatures>>,
    request_helper_recipe: RequestHelperRecipe,
}

impl ExactCompileContext {
    pub fn new(declarations: Arc<ExactDeclarationContext>) -> Self {
        Self {
            declarations,
            request_types: None,
            request_helper_recipe: RequestHelperRecipe::None,
        }
    }

    pub fn declarations(&self) -> &Arc<ExactDeclarationContext> {
        &self.declarations
    }

    pub fn request_types(&self) -> Option<&Arc<crate::checked_cell::RequestTypeSignatures>> {
        self.request_types.as_ref()
    }

    pub fn with_request_types(
        mut self,
        signatures: Arc<crate::checked_cell::RequestTypeSignatures>,
    ) -> Self {
        self.request_types = Some(signatures);
        self
    }

    pub fn with_request_helper_recipe(mut self, recipe: RequestHelperRecipe) -> Self {
        self.request_helper_recipe = recipe;
        self
    }

    pub fn request_helper_recipe(&self) -> RequestHelperRecipe {
        self.request_helper_recipe
    }

    pub(crate) fn with_declarations(mut self, declarations: Arc<ExactDeclarationContext>) -> Self {
        self.declarations = declarations;
        self
    }

    fn authorization(&self, purpose: Option<Value>) -> Option<Value> {
        match &self.request_types {
            Some(signatures) => Some(Value::Array(vec![
                text("request-types2"),
                signatures.authorization_value(),
                text(self.request_helper_recipe.as_str()),
                purpose.unwrap_or(Value::Null),
            ])),
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

fn original_products(entries: &[Arc<ArtifactEntry>]) -> Vec<&CertifiedRecoveryProduct> {
    entries
        .iter()
        .filter_map(|entry| match &entry.payload {
            ArtifactPayload::Original(product) => Some(product),
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
fn execution_scope_value(
    entries: &[Arc<ArtifactEntry>],
    root: &Path,
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
                || !required_graph.eligible_execution_root(&owner)
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
                    let path = root.join(format!("execution-{}.cbor", hex(&digest)));
                    // The request owner captures each distinct graph once. Its digest
                    // and exact original references are sealed by the scope manifest.
                    let mut file = std::fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(&path)?;
                    file.write_all(graph.bytes())?;
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
    fields[1] = text("8");
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
    originals: BTreeMap<
        crate::artifact_inventory::ArtifactId,
        crate::certified_products::RecoveryOriginalRequirements,
    >,
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
            inventory: ArtifactInventory::default().empty_view(),
            lexical: vec![],
        };
        let mut validation = PackageInterfaceValidation::default();
        let mut entries = Vec::new();
        let mut losses = Vec::new();
        let mut verified_modules = Vec::new();
        for reference in module_interfaces {
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
        let mut original_requirements = BTreeMap::new();
        for original in certified {
            context.admit_producer(original.producer_sha256)?;
            let product = original.product;
            let requirements = original.requirements;
            let sources = requirements
                .sources
                .iter()
                .map(|owner| identity(&owner.unit, &owner.module))
                .collect();
            if product.owner() != &requirements.owner {
                return Err(failure("recovered original owner differs").into());
            }
            let entry = ArtifactEntry::original(context.producer, product, sources)?;
            original_requirements.insert(entry.descriptor.id, requirements);
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
                ArtifactKind::LexicalJoin,
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
                ArtifactKind::ValueInterface,
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
            originals: original_requirements,
            interfaces,
        })
    }

    pub fn context(
        &self,
        ids: &[crate::artifact_inventory::ArtifactId],
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
        let mut owners = BTreeMap::new();
        for entry in &entries {
            let owner = entry.descriptor.owner.clone();
            if matches!(entry.payload, ArtifactPayload::Original(_)) || !owners.contains_key(&owner)
            {
                owners.insert(owner, entry);
            }
        }
        for (from, to, _) in &self.interfaces {
            if selected.contains(from) && !selected.contains(to) {
                return Err(failure("recovered interface closure is incomplete"));
            }
        }
        for id in ids {
            if let Some(original) = self.originals.get(id) {
                for required in &original.sources {
                    let entry = owners
                        .get(&identity(&required.unit, &required.module))
                        .ok_or_else(|| failure("native dependency artifact is missing"))?;
                    let ArtifactPayload::Original(product) = &entry.payload else {
                        return Err(failure("native source requires an original product"));
                    };
                    if product.owner() != required {
                        return Err(failure(
                            "native source owner differs from original certification",
                        ));
                    }
                }
                if original
                    .packages
                    .iter()
                    .any(|owner| owners.contains_key(owner))
                {
                    return Err(failure("home owner downgraded to package"));
                }
            }
        }
        let inventory = ArtifactInventory::default();
        let empty = inventory.empty_view();
        let mut context = ExactDeclarationContext {
            producer: self.producer,
            inventory: inventory.admit_shared(&empty, entries)?,
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

#[derive(Clone)]
pub(crate) struct ExactCompilationRequest {
    pub(crate) context: Arc<ExactDeclarationContext>,
    pub(crate) manifest: PathBuf,
    pub(crate) request_sha256: String,
    pub(crate) semantic_sha256: [u8; 32],
    pub(crate) producer_sha256: [u8; 32],
    pub(crate) artifacts: Vec<DeclarationArtifact>,
    pub(crate) groups: Arc<[PendingCertifiedGroup]>,
    // Only current-program source-selected support can add these roots.
    program_support: Option<ArtifactView>,
    program_source_lexical: Vec<ExactLexicalNode>,
    source_selected_support: BTreeSet<ExactModuleIdentity>,
    source_search_include: Option<Arc<[PathBuf]>>,
    checked_value_imports: crate::checked_cell::CheckedValueImportAuthority,
    generated_scaffold_imports: Option<GeneratedScaffoldImportAuthority>,
}

/// The compiler-only import belongs to a hash-sealed checked template and one
/// original native/interface owner. It never grants authored lexical visibility.
#[derive(Clone)]
struct GeneratedScaffoldImportAuthority {
    owner: tidepool_repr::execution_schema::CachedHomeOwner,
    protected_templates: Arc<[String]>,
}

const GENERATED_RESUME_IMPORT: &str = "import qualified Tidepool.Internal.Resume as TidepoolResume";

impl GeneratedScaffoldImportAuthority {
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
        if target != module_source
            || boot
            || qualifier != "none"
            || unit != self.owner.unit
            || module != self.owner.module
        {
            return false;
        }
        let lines = source
            .lines()
            .enumerate()
            .filter_map(|(line, text)| (text == GENERATED_RESUME_IMPORT).then_some(line + 1))
            .collect::<Vec<_>>();
        // GHC inserts admitted pragmas/imports before rendering the protected
        // recipe. Its captured occurrence is therefore derived from rendered
        // bytes, while both protected and rendered inputs must contain it once.
        if lines.len() != 1
            || !self.protected_templates.iter().any(|template| {
                template
                    .lines()
                    .filter(|line| *line == GENERATED_RESUME_IMPORT)
                    .count()
                    == 1
            })
        {
            return false;
        }
        context.artifact_view().entries().iter().any(|entry| {
            matches!(&entry.payload, ArtifactPayload::Original(product)
                if product.owner() == &self.owner)
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
    pub(crate) evidence: crate::cache::DependencyEvidence,
    pub(crate) evidence_bytes: Vec<u8>,
    pub(crate) exact_imports: BTreeMap<ExactModuleIdentity, Vec<ExactModuleIdentity>>,
    pub(crate) selected_originals:
        BTreeMap<ExactModuleIdentity, crate::execution_source::SourceSelectedOriginal>,
}

pub(crate) struct ExactProductAdmission<'a> {
    pub(crate) request: &'a ExactCompilationRequest,
    pub(crate) source: &'a ExactSourceAdmission,
}

impl ExactSourceAdmission {
    pub(crate) fn home_imports(
        &self,
    ) -> Result<BTreeMap<ExactModuleIdentity, Vec<ExactModuleIdentity>>, CompileError> {
        let source_paths = self
            .evidence
            .sources
            .iter()
            .map(|source| &source.path)
            .collect::<BTreeSet<_>>();
        let mut selected_owners = BTreeMap::new();
        for node in &self.evidence.modules {
            if selected_owners
                .insert((&node.unit, &node.module, node.boot, &node.source), node)
                .is_some()
            {
                return Err(failure("duplicate captured source import owner"));
            }
        }
        let mut imports = BTreeMap::new();
        for node in self.evidence.modules.iter().filter(|node| !node.boot) {
            if !source_paths.contains(&node.source) {
                return Err(failure(
                    "original source import owner lacks its captured source",
                ));
            }
            let owner = identity(&node.unit, &node.module);
            let mut requirements = self
                .exact_imports
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
            .map(ArtifactView::root_entries)
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
    ) -> Result<(), CompileError> {
        let request = tidepool_extract_cmd::ExtractRequest::decode(&command.request_bytes())
            .map_err(failure)?;
        let retained = certified_retained_generation_tags(
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
                self.generated_scaffold_imports = Some(GeneratedScaffoldImportAuthority {
                    owner: (*owner).clone(),
                    protected_templates: protected_templates.into(),
                });
            }
        }
        self
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
        // This request already owns authenticated materializations for its
        // baseline context. A later item adds immutable entries, while output
        // admission still revalidates the full consumed files for tampering.
        let baseline_ids = self
            .context
            .artifact_view()
            .artifact_ids()
            .into_iter()
            .collect::<BTreeSet<_>>();
        let current_entries = context.artifact_view().entries();
        let current_ids = current_entries
            .iter()
            .map(|entry| entry.descriptor.id)
            .collect::<BTreeSet<_>>();
        if !baseline_ids.is_subset(&current_ids) {
            return Err(failure("program context removed an admitted artifact"));
        }
        let new_entries = current_entries
            .iter()
            .filter(|entry| !baseline_ids.contains(&entry.descriptor.id))
            .cloned()
            .collect::<Vec<_>>();
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
        materialized
            .artifacts
            .extend(self.artifacts.iter().cloned());
        let certify_start = std::time::Instant::now();
        let products = new_entries
            .iter()
            .filter_map(|entry| match &entry.payload {
                ArtifactPayload::Original(product) => Some(product),
                _ => None,
            })
            .collect::<Vec<_>>();
        let available = original_products(&current_entries);
        let additional = certify_owned_products_in_context_with_validation(
            &products,
            &self.groups,
            &available,
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
        products: &[CertifiedRecoveryProduct],
        admissions: &[ExactSourceAdmission],
    ) -> Result<Arc<ExactDeclarationContext>, CompileError> {
        let mut imports = BTreeMap::new();
        let mut selected_originals = BTreeMap::new();
        for admission in admissions {
            for (owner, original) in &admission.selected_originals {
                if selected_originals
                    .insert(owner.clone(), original.clone())
                    .is_some_and(
                        |previous: crate::execution_source::SourceSelectedOriginal| {
                            previous.owner() != original.owner()
                                || previous.local_recipe_sha256() != original.local_recipe_sha256()
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
        let retained = context.artifact_view().entries_for_owners(
            products
                .iter()
                .map(|product| identity(&product.owner().unit, &product.owner().module)),
        );
        let fresh = products
            .iter()
            .filter(|product| {
                let owner = identity(&product.owner().unit, &product.owner().module);
                imports.contains_key(&owner) && !selected_originals.contains_key(&owner)
            })
            .collect::<Vec<_>>();
        for product in products {
            let owner = identity(&product.owner().unit, &product.owner().module);
            if imports.contains_key(&owner) {
                if let Some(selected) = selected_originals.get(&owner) {
                    let Some(entry) = retained.get(&owner) else {
                        return Err(failure(
                            "source-selected support is not an existing original",
                        ));
                    };
                    if !matches!(&entry.payload, ArtifactPayload::Original(original)
                        if original.owner() == selected.owner() && product.owner() == selected.owner())
                    {
                        return Err(failure(
                            "source-selected support has another original owner",
                        ));
                    }
                } else if retained.contains_key(&owner) {
                    return Err(failure(
                        "program support cannot select a retained hidden owner",
                    ));
                }
            } else if !retained.contains_key(&owner) {
                return Err(failure("program support lacks its fresh source admission"));
            }
        }
        let extend_start = std::time::Instant::now();
        let context = Arc::new((*context).clone().extend_checked_original_products(
            self.producer_sha256,
            products,
            &imports,
        )?);
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
            .map(|product| identity(&product.owner().unit, &product.owner().module))
            // This helper is selected by executable scaffolding. Its native
            // custody does not introduce an authored import into later cells.
            .filter(|owner| owner.unit != "main" || owner.module != "Tidepool.Internal.Resume")
            // Selected originals may be source roots without any fresh product;
            // their adjacency comes from the validated current-source receipt.
            .chain(selected_originals.keys().cloned())
            .collect::<Vec<_>>();
        let implementations = context
            .artifact_view()
            .descriptors()
            .into_iter()
            .map(|descriptor| (descriptor.owner, descriptor.kind))
            .collect();
        self.program_source_lexical = crate::declaration_join::source_lexical_surface(
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
                .map(|product| identity(&product.owner().unit, &product.owner().module))
                .chain(selected_originals.keys().cloned()),
        );
        for (owner, selected) in &selected_originals {
            let Some(entry) = entries.get(owner) else {
                return Err(failure(
                    "source-selected support lacks its original inventory entry",
                ));
            };
            if !matches!(&entry.payload, ArtifactPayload::Original(original) if original.owner() == selected.owner())
            {
                return Err(failure(
                    "source-selected support cannot replace an original",
                ));
            }
        }
        let fresh = context
            .artifact_view()
            .select_roots(entries.values().map(|entry| entry.descriptor.id).collect())?;
        self.program_support = Some(match &self.program_support {
            Some(previous) => previous.merge(&fresh)?,
            None => fresh,
        });
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
        use sha2::Digest;
        let expected: [u8; 32] = sha2::Sha256::digest(source.as_bytes()).into();
        self.validate_outputs(
            source_path
                .parent()
                .ok_or_else(|| failure("source has no directory"))?,
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
        );
        let entries = view.entries_for_owners(
            planned
                .original_home_imports()
                .filter(|(owner, _)| !retained.contains_key(*owner))
                .map(|(owner, _)| owner.clone()),
        );
        let support =
            view.select_roots(entries.values().map(|entry| entry.descriptor.id).collect())?;
        let mut request = self.clone();
        request.program_support = Some(match &self.program_support {
            Some(previous) => previous.merge(&support)?,
            None => support,
        });
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
        let context_validate_start = std::time::Instant::now();
        self.context.validate_artifacts(&self.artifacts)?;
        self.checked_value_imports.validate()?;
        if sha256(&std::fs::read(&self.manifest)?) != self.request_sha256 {
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
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<Result<Vec<_>, _>>()?;
        receipts.sort();
        if receipts.is_empty() || receipts.len() > 4096 {
            return Err(failure("missing or excessive successful compile receipts"));
        }
        let admitted = receipts
            .iter()
            .map(|path| self.validate_receipt(&path.join("receipt.cbor"), planned, context))
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

    fn validate_receipt(
        &self,
        path: &Path,
        planned: Option<&ExactModuleIdentity>,
        context: &ExactDeclarationContext,
    ) -> Result<ExactSourceAdmission, CompileError> {
        use sha2::Digest;
        let bytes = bounded_read(path, 4 * 1024 * 1024)?;
        let mut cursor = std::io::Cursor::new(&bytes);
        let value: Value = ciborium::de::from_reader(&mut cursor).map_err(failure)?;
        if cursor.position() != bytes.len() as u64 {
            return Err(failure("compile receipt has trailing bytes"));
        }
        let header = row(&value, 10)?;
        if string(&header[0])? != "TPEXACTCOMPILE"
            || string(&header[1])? != "2"
            || string(&header[2])? != self.request_sha256
            || string(&header[3])? != hex(&self.semantic_sha256)
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
        let source_bytes = bounded_read(&snapshot, 32 * 1024 * 1024)?;
        let source_sha256: [u8; 32] = sha2::Sha256::digest(&source_bytes).into();
        if string(&header[5])? != hex(&source_sha256) {
            return Err(failure("compile source snapshot changed"));
        }
        let source = std::str::from_utf8(&source_bytes).map_err(failure)?;
        let evidence_bytes = string(&header[7])?.as_bytes().to_vec();
        let evidence =
            crate::cache::DependencyEvidence::from_worker(&evidence_bytes, &source_path, source)
                .ok_or_else(|| {
                    failure("fresh compilation lacks complete tracked source evidence")
                })?;
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
            .map(ArtifactView::root_entries)
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
        let edges = list(&header[8], 4096)?;
        let mut source_selection_roots = Vec::new();
        for module in edges {
            let module = row(module, 4)?;
            let source_owner = identity(string(&module[0])?, string(&module[1])?);
            for edge in list(&module[3], 4096)? {
                let edge = row(edge, 4)?;
                if !boolean(&edge[2])? {
                    let qualifier =
                        crate::cache::ImportQualifier::try_from(string(&edge[0])?.to_owned())
                            .map_err(failure)?;
                    source_selection_roots.push((
                        source_owner.clone(),
                        qualifier,
                        identity(string(&edge[3])?, string(&edge[1])?),
                    ));
                }
            }
        }
        let source_selection = row(&header[9], 2)?;
        let claims = list(&source_selection[0], 4096)?;
        let selected_originals = if claims.is_empty() {
            if source_selection[1] != Value::Null {
                return Err(failure(
                    "empty original selection has nonempty source evidence",
                ));
            }
            BTreeMap::new()
        } else {
            let include = self.source_search_include.as_deref().ok_or_else(|| {
                failure("source-selected originals lack trusted current import roots")
            })?;
            let claims = claims
                .iter()
                .map(|claim| {
                    let claim = row(claim, 6)?;
                    Ok(crate::execution_source::SourceSelectedOriginalClaim {
                        owner: tidepool_repr::execution_schema::CachedHomeOwner {
                            unit: string(&claim[0])?.to_owned(),
                            module: string(&claim[1])?.to_owned(),
                            module_version: tidepool_repr::execution_schema::ModuleVersion(
                                crate::execution_source::parse_digest(string(&claim[2])?)?,
                            ),
                            skinny_iface_sha256: crate::execution_source::parse_digest(string(
                                &claim[3],
                            )?)?,
                            product_sha256: crate::execution_source::parse_digest(string(
                                &claim[4],
                            )?)?,
                        },
                        local_recipe_sha256: crate::execution_source::parse_digest(string(
                            &claim[5],
                        )?)?,
                    })
                })
                .collect::<Result<Vec<_>, CompileError>>()?;
            let selection_evidence: crate::cache::DependencyEvidence =
                serde_json::from_str(string(&source_selection[1])?).map_err(failure)?;
            let entries = context.artifact_view().entries();
            let originals = original_products(&entries);
            let independent = selected
                .iter()
                .map(|(unit, module)| identity(unit, module))
                .collect();
            crate::execution_source::validate_source_selected_originals(
                claims,
                &selection_evidence,
                crate::execution_source::SourceSelectionContext {
                    producer: self.producer_sha256,
                    originals: &originals,
                    include,
                    fresh: &evidence,
                    roots: &source_selection_roots,
                    independent: &independent,
                },
            )?
        };
        let mut seen = BTreeSet::new();
        let mut exact_imports = BTreeMap::new();
        for module in edges {
            let module = row(module, 4)?;
            let owner = (
                string(&module[0])?,
                string(&module[1])?,
                boolean(&module[2])?,
            );
            if !source_owners.contains(&owner) || !seen.insert(owner) {
                return Err(failure(
                    "exact import witness has another fresh source owner",
                ));
            }
            let mut imported = BTreeSet::new();
            let mut resolved = BTreeSet::new();
            for edge in list(&module[3], 4096)? {
                let edge = row(edge, 4)?;
                let qualifier = string(&edge[0])?;
                let name = string(&edge[1])?;
                let boot = boolean(&edge[2])?;
                let unit = string(&edge[3])?;
                let scaffold_import =
                    self.generated_scaffold_imports
                        .as_ref()
                        .is_some_and(|authority| {
                            evidence
                                .modules
                                .iter()
                                .find(|module| {
                                    module.unit == owner.0
                                        && module.module == owner.1
                                        && module.boot == owner.2
                                })
                                .is_some_and(|module| {
                                    // from_worker has already bound this marker to
                                    // the hash-verified receipt target snapshot.
                                    let module_source = if module.source
                                        == Path::new(crate::cache::GENERATED_SOURCE)
                                    {
                                        &source_path
                                    } else {
                                        &module.source
                                    };
                                    authority.permits(
                                        context,
                                        source,
                                        &source_path,
                                        module_source,
                                        unit,
                                        name,
                                        qualifier,
                                        boot,
                                    )
                                })
                        });
                if boot
                    || !(selected.contains(&(unit, name))
                        || scaffold_import
                        || selected_originals.contains_key(&identity(unit, name)))
                    || (qualifier != "none" && qualifier != format!("this:{unit}"))
                    || !imported.insert((qualifier, name, boot, unit))
                {
                    return Err(failure(format!(
                        "exact import witness leaves selected lexical graph: source {}:{}, import {unit}:{name}, qualifier {qualifier}, boot {boot}, selected {}",
                        owner.0,
                        owner.1,
                        selected.contains(&(unit, name)),
                    )));
                }
                resolved.insert(identity(unit, name));
            }
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
            selected_originals,
        })
    }
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
    let interface = match (&entry.descriptor.kind, &entry.payload) {
        (ArtifactKind::CanonicalModuleInterface, ArtifactPayload::Canonical(interface)) => {
            interface
        }
        (ArtifactKind::OriginalModule, ArtifactPayload::Original(product)) => product
            .module_interface()
            .ok_or_else(|| failure("native interface evidence is missing"))?,
        (ArtifactKind::LexicalJoin, ArtifactPayload::Interface(_, _)) => {
            return Ok(Value::Array(vec![text("join")]))
        }
        (ArtifactKind::ValueInterface, ArtifactPayload::Interface(_, _)) => {
            return Ok(Value::Array(vec![text("value")]))
        }
        _ => return Err(failure("interface evidence kind differs from payload")),
    };
    let reference = recovery_artifacts::materialize_module_interface(
        root,
        interface,
        validation,
        MaterializationMode::Scratch,
    )
    .map_err(failure)?;
    Ok(Value::Array(vec![
        text("module"),
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
        }
        .extend(authored, joins, lexical)
    }

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
        lexical: Vec<ExactLexicalNode>,
    ) -> Result<Self, CompileError> {
        Self::capture_recovery_inputs(
            root,
            products,
            module_interfaces,
            joins,
            values,
            Some((descriptors, dependencies)),
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
        inventory.context(
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
                ArtifactKind::LexicalJoin,
                join.context()
                    .interface_owners()
                    .into_iter()
                    .map(|interface| interface.owner)
                    .collect(),
            ));
        }
        self.inventory = self.inventory.inventory().admit(&self.inventory, entries)?;
        self.lexical = lexical;
        self.normalize()?;
        Ok(self)
    }

    /// Admit the original supporting homes sealed by the same checked-cell
    /// transaction. Lexical exposure remains a separate caller-selected graph.
    pub(crate) fn extend_checked_original_products(
        mut self,
        producer_sha256: [u8; 32],
        products: &[CertifiedRecoveryProduct],
        exact_imports: &BTreeMap<ExactModuleIdentity, Vec<ExactModuleIdentity>>,
    ) -> Result<Self, CompileError> {
        self.admit_producer(producer_sha256)?;
        if products.is_empty() {
            return Ok(self);
        }
        let existing = self.inventory.entries_for_owners(
            products
                .iter()
                .map(|product| identity(&product.owner().unit, &product.owner().module)),
        );
        let mut entries = Vec::new();
        let mut validation = PackageInterfaceValidation::default();
        for product in products {
            let owner = identity(&product.owner().unit, &product.owner().module);
            if let Some(entry) = existing.get(&owner) {
                if let ArtifactPayload::Canonical(interface) = &entry.payload {
                    if product.module_interface() != Some(interface) {
                        return Err(failure("supporting original differs from canonical module"));
                    }
                    entries.push(ArtifactEntry::original(
                        producer_sha256,
                        product.clone(),
                        interface
                            .requirements()
                            .keys()
                            .map(|(unit, module)| identity(unit, module))
                            .collect(),
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
            let mut requirements =
                crate::certified_products::original_home_requirements_with_validation(
                    product,
                    &mut validation,
                )
                .map_err(failure)?
                .into_iter()
                .map(|owner| identity(&owner.unit, &owner.module))
                .collect::<Vec<_>>();
            requirements.extend(exact_imports.get(&owner).into_iter().flatten().cloned());
            entries.push(ArtifactEntry::original(
                producer_sha256,
                product.clone(),
                requirements,
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
                ArtifactPayload::Interface(interface, ArtifactKind::LexicalJoin) => {
                    Some(interface.clone())
                }
                _ => None,
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
                ArtifactPayload::Interface(interface, ArtifactKind::ValueInterface) => {
                    Some(CertifiedValueInterface::from_admitted_interface(
                        interface.clone(),
                        entry.requirements.clone(),
                    ))
                }
                _ => None,
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
                ArtifactKind::ValueInterface,
                value.requirements().to_vec(),
            ));
        }
        self.inventory = self.inventory.inventory().admit(&self.inventory, entries)?;
        self.lexical = lexical;
        self.normalize()?;
        Ok(self)
    }

    pub(crate) fn extend_retained_value_artifacts(
        mut self,
        values: &[Arc<crate::checked_cell::CheckedValueArtifact>],
    ) -> Result<Self, CompileError> {
        let mut lexical = self
            .lexical
            .iter()
            .map(|node| (node.owner.clone(), node.imports.clone()))
            .collect::<BTreeMap<_, _>>();
        for value in values {
            let interface = value
                .certified_interface()
                .ok_or_else(|| failure("retained value lacks same-compiler certification"))?;
            self.admit_producer(interface.interface().toolchain_identity_sha256())?;
            self.inventory = self.inventory.merge(value.artifact_view()?)?;
            for node in value.source_lexical() {
                if lexical
                    .insert(node.owner.clone(), node.imports.clone())
                    .is_some_and(|previous| previous != node.imports)
                {
                    return Err(failure(
                        "retained value changes its source-selected lexical graph",
                    ));
                }
            }
        }
        self.lexical = lexical
            .into_iter()
            .map(|(owner, imports)| ExactLexicalNode { owner, imports })
            .collect();
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
        let mut selected = BTreeMap::new();
        for node in self.lexical.iter().chain(support) {
            if node.owner.module.starts_with("Tidepool.Session.") {
                continue;
            }
            if selected
                .insert(node.owner.clone(), node.imports.clone())
                .is_some_and(|previous| previous != node.imports)
            {
                return Err(failure(
                    "value source surface has conflicting selected edges",
                ));
            }
        }
        let roots = selected
            .keys()
            .filter(|owner| retained.contains(*owner))
            .cloned()
            .collect::<Vec<_>>();
        let implementations = self
            .artifact_view()
            .descriptors()
            .into_iter()
            .map(|descriptor| (descriptor.owner, descriptor.kind))
            .collect();
        let lexical = crate::declaration_join::source_lexical_surface(
            &roots,
            &selected,
            &[],
            &implementations,
        )?
        .lexical;
        let entries = self
            .artifact_view()
            .entries_for_owners(lexical.iter().map(|node| node.owner.clone()));
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
                .collect(),
        )?;
        Ok((view, lexical))
    }

    /// Reuse only the initial value interfaces selected by the sealed checked
    /// cell. Their dependency closure supplies hydration, never lexical names.
    pub(crate) fn extend_checked_value_input_context<'a>(
        mut self,
        initial: &Self,
        values: impl Iterator<Item = (tidepool_repr::SessionModule, &'a [u8])>,
    ) -> Result<Self, CompileError> {
        let values = values
            .map(|(owner, bytes)| (identity("main", &owner.module_name()), bytes))
            .collect::<BTreeMap<_, _>>();
        let entries = initial.inventory.entries_for_owners(values.keys().cloned());
        let mut roots = Vec::new();
        for (owner, entry) in entries {
            let ArtifactPayload::Interface(interface, ArtifactKind::ValueInterface) =
                &entry.payload
            else {
                return Err(failure("checked input has another artifact kind"));
            };
            if interface.interface_bytes() != values[&owner] {
                return Err(failure("checked input differs from its sealed interface"));
            }
            self.admit_producer(interface.toolchain_identity_sha256())?;
            roots.push(entry.descriptor.id);
        }
        if !roots.is_empty() {
            self.inventory = self
                .inventory
                .merge(&initial.inventory.select_roots(roots)?)?;
            self.normalize()?;
        }
        Ok(self)
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
        let value = Value::Array(vec![
            text("TPEXACTCONTEXT"),
            text("2"),
            text(hex(&sha2::Sha256::digest(
                serde_json::to_vec(&(
                    metadata
                        .artifacts
                        .values()
                        .map(|entry| &entry.descriptor)
                        .collect::<Vec<_>>(),
                    metadata.dependencies(),
                ))
                .expect("inventory encoding"),
            )
            .into())),
            text(hex(&self.producer)),
            Value::Array(
                metadata
                    .entries
                    .values()
                    .filter_map(|entry| match &entry.payload {
                        ArtifactPayload::Original(product) => Some((&entry.descriptor, product)),
                        _ => None,
                    })
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
                        ArtifactPayload::Interface(interface, ArtifactKind::LexicalJoin) => {
                            Some((&entry.descriptor, interface))
                        }
                        _ => None,
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
        ]);
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
            let (iface, packages) = if let ArtifactPayload::Original(product) = &entry.payload {
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
            } else if let ArtifactPayload::Canonical(interface) = &entry.payload {
                if artifact.product.is_some() {
                    return Err(failure("type interface has an executable product"));
                }
                (
                    interface.interface_bytes(),
                    interface.package_imports_bytes(),
                )
            } else {
                let ArtifactPayload::Interface(
                    join,
                    ArtifactKind::LexicalJoin | ArtifactKind::ValueInterface,
                ) = &entry.payload
                else {
                    return Err(failure("synthetic anchor is missing"));
                };
                if artifact.product.is_some() {
                    return Err(failure("synthetic anchor has an implementation product"));
                }
                (join.interface_bytes(), join.package_imports_bytes())
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

    fn admit_producer(&mut self, producer: [u8; 32]) -> Result<(), CompileError> {
        if producer == [0; 32] || (self.producer != [0; 32] && producer != self.producer) {
            return Err(failure("producer identity differs"));
        }
        self.producer = producer;
        Ok(())
    }

    fn normalize(&mut self) -> Result<(), CompileError> {
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
        if lexical_owners.len() != self.lexical.len()
            || self.lexical.iter().any(|node| {
                !owners.contains(&node.owner)
                    || node
                        .imports
                        .iter()
                        .any(|owner| !lexical_owners.contains(owner))
                    || node.imports.iter().collect::<BTreeSet<_>>().len() != node.imports.len()
            })
        {
            return Err(failure("invalid selected lexical graph"));
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

    pub(crate) fn prepare_compilation_with_authorization(
        self: &Arc<Self>,
        root: &Path,
        producer: &[u8],
        authorization: Option<Value>,
    ) -> Result<ExactCompilationRequest, CompileError> {
        let metadata = self.inventory.metadata_snapshot();
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
        // Package reads share one synchronous preparation snapshot. It does not
        // escape this stage; post-worker verification opens a fresh snapshot.
        let mut validation = PackageInterfaceValidation::default();
        let entries = metadata.entries.values().cloned().collect::<Vec<_>>();
        let context_bytes = materialization_bytes(&entries);
        let materialize_start = std::time::Instant::now();
        let (materialized, _) = self.materialize_entries_with_validation(
            root,
            &entries,
            &mut validation,
            MaterializationMode::Scratch,
        )?;
        crate::timing::record_stage(
            crate::timing::NO_NODE,
            crate::timing::NO_ROUND,
            "exact.context_materialize",
            materialize_start.elapsed(),
            context_bytes,
        );
        let context_validate_start = std::time::Instant::now();
        self.validate_artifacts_from_metadata(&materialized.artifacts, metadata)?;
        crate::timing::record_stage(
            crate::timing::NO_NODE,
            crate::timing::NO_ROUND,
            "exact.context_validate",
            context_validate_start.elapsed(),
            0,
        );
        let groups_start = std::time::Instant::now();
        let products = original_products(&entries);
        let groups = certify_owned_products_in_context_with_validation(
            &products,
            &[],
            &products,
            &mut validation,
        )
        .map_err(failure)?;
        crate::timing::record_stage(
            crate::timing::NO_NODE,
            crate::timing::NO_ROUND,
            "exact.inherited_groups",
            groups_start.elapsed(),
            0,
        );
        let artifacts_by_owner = materialized
            .artifacts
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
        let execution_scope = execution_scope_value(&entries, root)?;
        let fields = vec![
            text("TPEXACTSCOPE"),
            text("2"),
            text(hex(&semantic_sha256)),
            text(
                crate::artifact_inventory::CanonicalProducerIdentity::from_producer_bytes(producer)
                    .hex(),
            ),
            Value::Array(
                materialized
                    .artifacts
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
                            text(sha256(&std::fs::read(packages)?)),
                            scope_interface_evidence(
                                metadata
                                    .entries
                                    .get(&identity(&iface.unit, &iface.module))
                                    .ok_or_else(|| {
                                        failure("interface evidence owner is missing")
                                    })?,
                                root,
                                &mut validation,
                            )?,
                        ]))
                    })
                    .collect::<Result<Vec<_>, CompileError>>()?,
            ),
            Value::Array(
                materialized
                    .lexical
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
        Ok(ExactCompilationRequest {
            context: self.clone(),
            manifest,
            request_sha256: sha256(&bytes),
            semantic_sha256,
            producer_sha256:
                crate::artifact_inventory::CanonicalProducerIdentity::from_producer_bytes(producer)
                    .sha256(),
            artifacts: materialized.artifacts,
            groups: groups.into(),
            program_support: None,
            program_source_lexical: Vec::new(),
            source_selected_support: BTreeSet::new(),
            source_search_include: None,
            checked_value_imports: Default::default(),
            generated_scaffold_imports: None,
        })
    }
}

pub(crate) fn certified_product_artifact_view(
    producer: [u8; 32],
    products: &[CertifiedRecoveryProduct],
    interfaces: &[crate::certified_products::CertifiedModuleInterface],
    baseline: Option<&ExactDeclarationContext>,
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
    for product in products {
        entries.push(ArtifactEntry::original(
            producer,
            product.clone(),
            Vec::new(),
        )?);
    }
    view.inventory().admit(&view, entries)
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
    let selected = compiled.entries_for_owners(
        products
            .iter()
            .map(|product| identity(&product.owner().unit, &product.owner().module)),
    );
    let view = view.merge(
        &compiled.select_roots(selected.values().map(|entry| entry.descriptor.id).collect())?,
    )?;
    let mut entries = Vec::new();
    for product in products {
        let owner = identity(&product.owner().unit, &product.owner().module);
        let requirements = interfaces
            .iter()
            .find(|interface| interface.owner == owner)
            .ok_or_else(|| failure("original interface metadata missing"))?
            .requirements
            .clone();
        entries.push(ArtifactEntry::original(
            producer,
            product.clone(),
            requirements,
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
            ArtifactKind::LexicalJoin,
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

#[cfg(test)]
mod tests {
    use super::*;

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
        let interface = format!("{module} interface").into_bytes();
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

    // Retain the pre-snapshot wire-v2 encoder as an independent compatibility oracle.
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
    fn metadata_identity_preserves_existing_v2_canonical_encoding() {
        let (context, _) = metadata_fixture();
        assert_eq!(context.semantic_sha256(), legacy_semantic_sha256(&context));
        let mut reordered = context.as_ref().clone();
        reordered.lexical.reverse();
        assert_eq!(
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
                &BTreeMap::new(),
            )
            .unwrap();
        assert_eq!(
            extended.semantic_sha256(),
            legacy_semantic_sha256(&extended)
        );
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
        let evidence = crate::cache::DependencyEvidence {
            version: 4,
            cache_safe: true,
            selection_complete: true,
            sources,
            resolutions: vec![],
            packages: vec![],
            modules: nodes,
        };
        ExactSourceAdmission {
            witness: ExactSourceWitness {
                source_path: root.join("Target.hs"),
                source_sha256: [3; 32],
            },
            evidence_bytes: serde_json::to_vec(&evidence).unwrap(),
            evidence,
            exact_imports: BTreeMap::new(),
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
                        Vec::new(),
                    )
                    .unwrap(),
                    ArtifactEntry::original(
                        producer_sha256,
                        crate::certified_products::fixture_finalized_product(
                            support_product("Beta"),
                            producer_sha256,
                        ),
                        vec![identity("fixture", "Alpha")],
                    )
                    .unwrap(),
                    ArtifactEntry::interface(
                        interface("Joined"),
                        ArtifactKind::LexicalJoin,
                        vec![identity("fixture", "Alpha"), identity("fixture", "Beta")],
                    ),
                    ArtifactEntry::interface(
                        interface("Value"),
                        ArtifactKind::ValueInterface,
                        vec![identity("fixture", "Alpha")],
                    ),
                ],
            )
            .unwrap();
        let context = ExactDeclarationContext {
            producer: producer_sha256,
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
        context.clone().normalize().unwrap();
        (Arc::new(context), producer)
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
        wrong_requirements[1].interface.requirements.clear();
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
        let ArtifactPayload::Interface(interface, _) = &entry.payload else {
            unreachable!()
        };
        *entry = ArtifactEntry::interface(
            interface.clone(),
            ArtifactKind::OriginalModule,
            entry.requirements.clone(),
        );
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
        assert_eq!(after.graph_visits - before.graph_visits, 4);
        assert_eq!(after.view_queries - before.view_queries, 1);
        assert_eq!(request.semantic_sha256, expected);
        let value: Value =
            ciborium::de::from_reader(std::fs::read(&request.manifest).unwrap().as_slice())
                .unwrap();
        let fields = row(&value, 8).unwrap();
        assert_eq!(string(&fields[2]).unwrap(), hex(&expected));
        assert_eq!(row(&fields[7], 2).unwrap()[1], text(hex(&expected)));
        std::fs::write(&request.artifacts[0].interface.path, b"late corruption").unwrap();
        assert!(request
            .context
            .validate_artifacts(&request.artifacts)
            .is_err());
    }

    #[test]
    fn metadata_identity_is_path_independent_and_tracks_graph_selection() {
        let (context, _) = metadata_fixture();
        let expected = context.semantic_sha256();
        let before = context.inventory.inventory().metrics();
        assert_eq!(context.semantic_sha256(), expected);
        let after = context.inventory.inventory().metrics();
        assert_eq!(after.graph_visits - before.graph_visits, 4);
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
                &BTreeMap::new(),
            )
            .unwrap();
        assert_ne!(extended.semantic_sha256(), expected);
        let mut entries = context
            .inventory
            .entries()
            .iter()
            .map(|entry| entry.as_ref().clone())
            .collect::<Vec<_>>();
        entries
            .iter_mut()
            .find(|entry| entry.descriptor.owner.module == "Beta")
            .unwrap()
            .requirements
            .clear();
        let inventory = ArtifactInventory::default();
        let mut changed = context.as_ref().clone();
        changed.inventory = inventory.admit(&inventory.empty_view(), entries).unwrap();
        assert_ne!(changed.semantic_sha256(), expected);
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
            program_support: None,
            program_source_lexical: Vec::new(),
            source_selected_support: BTreeSet::new(),
            source_search_include: None,
            checked_value_imports: Default::default(),
            generated_scaffold_imports: None,
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
            text("2"),
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
        let context = Arc::new(ExactDeclarationContext {
            producer: [7; 32],
            inventory: inventory
                .admit_shared(
                    &inventory.empty_view(),
                    vec![
                        execution_entry(owners[0].clone(), Arc::clone(&graph)),
                        execution_entry(owners[1].clone(), Arc::clone(&graph)),
                    ],
                )
                .unwrap(),
            lexical: vec![],
        });
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
            claims.push(Value::Array(vec![
                text(&owner.unit),
                text(&owner.module),
                text(hex(&owner.module_version.0)),
                text(hex(&owner.skinny_iface_sha256)),
                text(hex(&owner.product_sha256)),
                text(hex(&graph.digest())),
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
            .entries_for_owners([identity("main", "A"), identity("main", "B")].into_iter());
        let products =
            ["A", "B"].map(
                |module| match &originals[&identity("main", module)].payload {
                    ArtifactPayload::Original(product) => product.clone(),
                    _ => panic!("original fixture"),
                },
            );
        let effective = request
            .admit_program_support(Arc::clone(&context), &products, &[admission])
            .unwrap();
        let after = effective
            .artifact_view()
            .entries_for_owners([identity("main", "A"), identity("main", "B")].into_iter());
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
                request.program_support.as_ref().unwrap(),
                request.program_source_lexical(),
            )
            .unwrap();
        assert_eq!(lexical, request.program_source_lexical());
        assert_eq!(
            request
                .program_support
                .as_ref()
                .unwrap()
                .root_entries()
                .iter()
                .map(|entry| entry.descriptor.owner.clone())
                .collect::<Vec<_>>(),
            vec![identity("main", "A"), identity("main", "B")]
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
    fn source_selected_original_receipt_refuses_owner_recipe_version_and_unreachable_claims() {
        let directory = tempfile::tempdir().unwrap();
        let (request, context, receipt) = source_selected_receipt(directory.path(), false, None);
        let valid = read_receipt(&receipt);
        for index in 2..6 {
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
        for version in ["1", "3"] {
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
    fn source_selected_original_receipt_revalidates_original_package_interface_bytes() {
        let directory = tempfile::tempdir().unwrap();
        let package = directory.path().join("package.hi");
        std::fs::write(&package, b"original package interface").unwrap();
        let (graph, owners) = crate::execution_source::test_graph(directory.path());
        let graph = crate::execution_source::test_graph_with_package_witness(&graph, &package);
        let (request, context, receipt) =
            source_selected_receipt_with_graph(directory.path(), false, None, graph, owners);
        request.validate_receipt(&receipt, None, &context).unwrap();
        std::fs::write(package, b"changed package interface").unwrap();
        assert!(request.validate_receipt(&receipt, None, &context).is_err());
    }

    #[test]
    fn source_selected_original_receipt_authenticates_ultimate_fresh_recipe_in_original_chain() {
        let directory = tempfile::tempdir().unwrap();
        let (graph, owners) = crate::execution_source::test_graph(directory.path());
        let forwarded = crate::execution_source::test_graph_requiring_original(
            &graph,
            &owners[1],
            graph.digest(),
        );
        let (mut request, _, receipt) = source_selected_receipt_with_graph(
            directory.path(),
            true,
            None,
            Arc::clone(&graph),
            owners.clone(),
        );
        let inventory = ArtifactInventory::default();
        let context = Arc::new(ExactDeclarationContext {
            producer: [7; 32],
            inventory: inventory
                .admit_shared(
                    &inventory.empty_view(),
                    vec![
                        execution_entry(owners[0].clone(), Arc::clone(&forwarded)),
                        execution_entry(owners[1].clone(), Arc::clone(&graph)),
                    ],
                )
                .unwrap(),
            lexical: vec![],
        });
        request.context = Arc::clone(&context);
        let mut valid = read_receipt(&receipt);
        let selection = valid.as_array_mut().unwrap()[9].as_array_mut().unwrap();
        selection[0].as_array_mut().unwrap()[0]
            .as_array_mut()
            .unwrap()[5] = text(hex(&forwarded.digest()));
        let mut evidence: crate::cache::DependencyEvidence =
            serde_json::from_str(selection[1].as_text().unwrap()).unwrap();
        selection[1] = text(serde_json::to_string(&evidence).unwrap());
        write_receipt(&receipt, &valid);
        request.validate_receipt(&receipt, None, &context).unwrap();
        let mut omitted = valid.clone();
        evidence.modules[0].imports.clear();
        evidence.resolutions.retain(|row| row.module != "B");
        omitted.as_array_mut().unwrap()[9].as_array_mut().unwrap()[1] =
            text(serde_json::to_string(&evidence).unwrap());
        write_receipt(&receipt, &omitted);
        assert!(
            request.validate_receipt(&receipt, None, &context).is_err(),
            "current source must report the authenticated original exact import"
        );
        let mut extra = valid.clone();
        let mut evidence: crate::cache::DependencyEvidence = serde_json::from_str(
            extra.as_array().unwrap()[9].as_array().unwrap()[1]
                .as_text()
                .unwrap(),
        )
        .unwrap();
        evidence.modules[0]
            .imports
            .push(crate::cache::ModuleImportEvidence {
                qualifier: crate::cache::ImportQualifier::Unqualified,
                module: "Unadmitted".into(),
                boot: false,
                selected: None,
            });
        extra.as_array_mut().unwrap()[9].as_array_mut().unwrap()[1] =
            text(serde_json::to_string(&evidence).unwrap());
        write_receipt(&receipt, &extra);
        assert!(
            request.validate_receipt(&receipt, None, &context).is_err(),
            "a retained exact edge cannot authorize another ordinary import"
        );
        let mut wrong = valid;
        wrong.as_array_mut().unwrap()[9].as_array_mut().unwrap()[0]
            .as_array_mut()
            .unwrap()[1]
            .as_array_mut()
            .unwrap()[5] = text(hex(&forwarded.digest()));
        write_receipt(&receipt, &wrong);
        assert!(request.validate_receipt(&receipt, None, &context).is_err(),
            "a consumer's graph digest cannot replace the dependency's ultimate fresh source recipe");
    }

    #[test]
    fn source_selected_original_receipt_distinguishes_equivalent_available_and_causal_recipes() {
        let directory = tempfile::tempdir().unwrap();
        let (original, owners) = crate::execution_source::test_graph(directory.path());
        let (parent, _) =
            crate::execution_source::test_graph_with_local_source_dependency(directory.path());
        let (mut request, _, receipt) = source_selected_receipt_with_graph(
            directory.path(),
            true,
            None,
            Arc::clone(&parent),
            owners.clone(),
        );
        let inventory = ArtifactInventory::default();
        let context = Arc::new(ExactDeclarationContext {
            producer: [7; 32],
            inventory: inventory
                .admit_shared(
                    &inventory.empty_view(),
                    vec![
                        execution_entry(owners[0].clone(), Arc::clone(&parent)),
                        execution_entry(owners[1].clone(), Arc::clone(&original)),
                    ],
                )
                .unwrap(),
            lexical: vec![],
        });
        request.context = Arc::clone(&context);
        request.validate_receipt(&receipt, None, &context).unwrap();
        let mut equivalent = read_receipt(&receipt);
        equivalent.as_array_mut().unwrap()[9]
            .as_array_mut()
            .unwrap()[0]
            .as_array_mut()
            .unwrap()[1]
            .as_array_mut()
            .unwrap()[5] = text(hex(&original.digest()));
        write_receipt(&receipt, &equivalent);
        assert!(
            request.validate_receipt(&receipt, None, &context).is_err(),
            "an equivalent B recipe merely available in custody was not reached from A"
        );
        let fields = equivalent.as_array_mut().unwrap();
        fields[8].as_array_mut().unwrap()[0].as_array_mut().unwrap()[3]
            .as_array_mut()
            .unwrap()
            .push(Value::Array(vec![
                text("none"),
                text("B"),
                Value::Bool(false),
                text("main"),
            ]));
        let mut fresh: crate::cache::DependencyEvidence =
            serde_json::from_str(fields[7].as_text().unwrap()).unwrap();
        let source = "module Consumer where\nimport A\nimport B\n";
        std::fs::write(Path::new(fields[4].as_text().unwrap()), source).unwrap();
        std::fs::write(Path::new(fields[6].as_text().unwrap()), source).unwrap();
        fields[5] = text(sha256(source.as_bytes()));
        fresh.sources[0].sha256 = sha256(source.as_bytes());
        fields[7] = text(serde_json::to_string(&fresh).unwrap());
        write_receipt(&receipt, &equivalent);
        request.validate_receipt(&receipt, None, &context).unwrap();
    }

    #[test]
    fn checked_value_import_authority_keeps_hidden_and_future_owners_unselected() {
        let root = tempfile::tempdir().unwrap();
        let context = Arc::new(
            ExactDeclarationContext::new(&[], &[], vec![])
                .unwrap()
                .extend_checked_original_products(
                    [2; 32],
                    &[support_product("Hidden")],
                    &BTreeMap::new(),
                )
                .unwrap(),
        );
        let owner = tidepool_repr::SessionModule::val(tidepool_repr::Generation(7));
        let inputs = crate::checked_cell::CheckedValueInputs::capture(vec![(
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

    #[test]
    fn program_support_retains_selected_home_edges_without_public_exposure() {
        let directory = tempfile::tempdir().unwrap();
        let hidden = support_product("Hidden");
        let baseline = Arc::new(
            ExactDeclarationContext::new(&[], &[], vec![])
                .unwrap()
                .extend_checked_original_products([2; 32], &[hidden], &BTreeMap::new())
                .unwrap(),
        );
        let mut request = program_request(directory.path(), baseline.clone());
        let admission = support_admission(directory.path());
        let context = request
            .admit_program_support(
                baseline.clone(),
                &[
                    support_product("InstanceOwner"),
                    support_product("InstanceRelay"),
                ],
                &[admission],
            )
            .unwrap();
        assert!(context.lexical_graph().is_empty());
        assert!(baseline.lexical_graph().is_empty());
        let relay = context
            .artifact_view()
            .entries_for_owners(std::iter::once(identity("fixture", "InstanceRelay")));
        assert_eq!(
            relay[&identity("fixture", "InstanceRelay")].requirements,
            vec![identity("fixture", "InstanceOwner")]
        );
        let effective = request
            .in_program_context(&directory.path().join("program-inputs"), context)
            .unwrap();
        let receipt = import_receipt(directory.path(), &effective, "InstanceRelay");
        assert!(effective
            .validate_receipt(&receipt, None, &effective.context)
            .is_ok());
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
                .extend_checked_original_products([2; 32], &products, &BTreeMap::new())
                .unwrap(),
        );
        let mut request = program_request(directory.path(), baseline.clone());
        assert!(request
            .admit_program_support(baseline, &products, &[support_admission(directory.path())])
            .is_err());
        let empty = Arc::new(ExactDeclarationContext::new(&[], &[], vec![]).unwrap());
        let mut request = program_request(directory.path(), empty.clone());
        let mut forged = support_admission(directory.path());
        forged.evidence.modules[0].source = directory.path().join("AnotherOwner.hs");
        assert!(request
            .admit_program_support(empty, &products, &[forged])
            .is_err());
    }

    #[test]
    fn program_support_refuses_conflicting_selected_edges() {
        let directory = tempfile::tempdir().unwrap();
        let empty = Arc::new(ExactDeclarationContext::new(&[], &[], vec![]).unwrap());
        let mut request = program_request(directory.path(), empty.clone());
        let original = support_admission(directory.path());
        let mut changed = support_admission(directory.path());
        changed.evidence.modules[1].imports.clear();
        assert!(request
            .admit_program_support(
                empty,
                &[
                    support_product("InstanceOwner"),
                    support_product("InstanceRelay")
                ],
                &[original, changed]
            )
            .is_err());
    }

    #[test]
    fn program_support_dependency_closure_does_not_select_hidden_names() {
        let directory = tempfile::tempdir().unwrap();
        let hidden = support_product("Hidden");
        let context = ExactDeclarationContext::new(&[], &[], vec![])
            .unwrap()
            .extend_checked_original_products([2; 32], &[hidden], &BTreeMap::new())
            .unwrap();
        let context = Arc::new(
            context
                .extend_checked_original_products(
                    [2; 32],
                    &[support_product("InstanceRelay")],
                    &BTreeMap::from([(
                        identity("fixture", "InstanceRelay"),
                        vec![identity("fixture", "Hidden")],
                    )]),
                )
                .unwrap(),
        );
        let entries = context
            .artifact_view()
            .entries_for_owners(std::iter::once(identity("fixture", "InstanceRelay")));
        let mut request = program_request(directory.path(), context.clone());
        request.program_support = Some(
            context
                .artifact_view()
                .select_roots(vec![
                    entries[&identity("fixture", "InstanceRelay")].descriptor.id,
                ])
                .unwrap(),
        );
        assert_eq!(
            request
                .program_support
                .as_ref()
                .unwrap()
                .descriptors()
                .len(),
            2
        );
        let receipt = import_receipt(directory.path(), &request, "InstanceRelay");
        assert!(request.validate_receipt(&receipt, None, &context).is_ok());
        let receipt = import_receipt(directory.path(), &request, "Hidden");
        assert!(request.validate_receipt(&receipt, None, &context).is_err());
        assert!(context.lexical_graph().is_empty());
        let (retained, lexical) = context
            .retain_value_source_surface(request.program_support.as_ref().unwrap(), &[])
            .unwrap();
        assert_eq!(retained.descriptors().len(), 2);
        assert!(lexical.is_empty());
    }

    #[test]
    fn retained_value_source_surface_preserves_selected_support_and_refuses_hidden_owners() {
        let directory = tempfile::tempdir().unwrap();
        let baseline = Arc::new(
            ExactDeclarationContext::new(&[], &[], vec![])
                .unwrap()
                .extend_checked_original_products(
                    [2; 32],
                    &[support_product("Hidden")],
                    &BTreeMap::new(),
                )
                .unwrap(),
        );
        let mut request = program_request(directory.path(), baseline.clone());
        let context = request
            .admit_program_support(
                baseline,
                &[
                    support_product("InstanceOwner"),
                    support_product("InstanceRelay"),
                ],
                &[support_admission(directory.path())],
            )
            .unwrap();
        assert!(context.lexical_graph().is_empty());
        let owner = identity("fixture", "InstanceRelay");
        let entries = context
            .artifact_view()
            .entries_for_owners(std::iter::once(owner.clone()));
        let value = context
            .artifact_view()
            .select_roots(vec![entries[&owner].descriptor.id])
            .unwrap();
        let (view, lexical) = context
            .retain_value_source_surface(&value, request.program_source_lexical())
            .unwrap();
        assert_eq!(view.descriptors().len(), 2);
        let later = (*context)
            .clone()
            .extend_checked_original_products(
                [2; 32],
                &[support_product("LaterSupport")],
                &BTreeMap::new(),
            )
            .unwrap();
        let mut later_support = request.program_source_lexical().to_vec();
        later_support.push(ExactLexicalNode {
            owner: identity("fixture", "LaterSupport"),
            imports: vec![],
        });
        let (earlier_view, earlier_lexical) = later
            .retain_value_source_surface(&value, &later_support)
            .unwrap();
        assert_eq!(earlier_view.descriptors().len(), 2);
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
                .extend_checked_original_products(
                    [2; 32],
                    std::slice::from_ref(&hidden),
                    &BTreeMap::new(),
                )
                .unwrap(),
        );
        let mut request = program_request(directory.path(), baseline.clone());
        let context = request
            .admit_program_support(
                baseline,
                &[hidden.clone(), owner.clone(), relay.clone()],
                &[support_admission(directory.path())],
            )
            .unwrap();
        assert_eq!(
            request
                .program_support
                .as_ref()
                .unwrap()
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
        later.evidence.modules = vec![crate::cache::ModuleEvidence {
            unit: "fixture".into(),
            module: "Additional".into(),
            boot: false,
            source: path.clone(),
            imports: vec![],
            product: crate::cache::ProductAvailability::Ready,
        }];
        later.evidence.sources = vec![crate::cache::SourceEvidence {
            path,
            sha256: sha256(b"module Additional where\n"),
        }];
        later.exact_imports.insert(
            identity("fixture", "Additional"),
            vec![identity("fixture", "InstanceRelay")],
        );
        let context = request
            .admit_program_support(
                context,
                &[hidden.clone(), owner, relay, support_product("Additional")],
                &[later],
            )
            .unwrap();
        assert_eq!(
            request
                .program_support
                .as_ref()
                .unwrap()
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
        let altered = CertifiedRecoveryProduct::from_certification(
            hidden.owner().clone(),
            b"different inherited interface".to_vec(),
            hidden.product_bytes().to_vec(),
            hidden.package_imports_bytes().to_vec(),
            hidden.certification_bytes().to_vec(),
        );
        assert!(request
            .admit_program_support(context, &[altered], &[])
            .is_err());
    }

    #[test]
    fn retained_value_source_surface_does_not_promote_compiler_scaffold_support() {
        let directory = tempfile::tempdir().unwrap();
        let baseline = Arc::new(ExactDeclarationContext::new(&[], &[], vec![]).unwrap());
        let mut request = program_request(directory.path(), baseline.clone());
        let mut admission = support_admission(directory.path());
        admission.evidence.modules.truncate(1);
        let source = &mut admission.evidence.modules[0];
        source.unit = "main".into();
        source.module = "Tidepool.Internal.Resume".into();
        source.imports.clear();
        let context = request
            .admit_program_support(
                baseline,
                &[support_product_in_unit("main", "Tidepool.Internal.Resume")],
                &[admission],
            )
            .unwrap();
        assert!(request.program_source_lexical().is_empty());
        let native = request.program_support.as_ref().unwrap();
        assert_eq!(native.descriptors().len(), 2);
        assert!(context
            .retain_value_source_surface(native, request.program_source_lexical())
            .unwrap()
            .1
            .is_empty());
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
        let mut admission = support_admission(directory.path());
        admission
            .evidence
            .modules
            .push(admission.evidence.modules[0].clone());
        assert!(admission.home_imports().is_err());
    }

    #[test]
    fn checked_input_context_retains_only_selected_value_closure_without_lexical_names() {
        let owner = tidepool_repr::SessionModule::val(tidepool_repr::Generation(7));
        let evidence = support_product_in_unit("main", &owner.module_name());
        let value = Arc::new(
            CertifiedValueInterface::from_checked_compilation(
                [2; 32],
                identity("main", &owner.module_name()),
                evidence.interface_bytes().to_vec(),
                evidence.package_imports_bytes().to_vec(),
                vec![identity("fixture", "Hidden")],
            )
            .unwrap(),
        );
        let initial = ExactDeclarationContext::new(&[], &[], vec![])
            .unwrap()
            .extend_checked_original_products(
                [2; 32],
                &[support_product("Hidden"), support_product("Unrelated")],
                &BTreeMap::new(),
            )
            .unwrap()
            .extend_with_value_interfaces(&[value], vec![])
            .unwrap();
        let current = ExactDeclarationContext::new(&[], &[], vec![]).unwrap();
        let enriched = current
            .clone()
            .extend_checked_value_input_context(
                &initial,
                std::iter::once((owner, evidence.interface_bytes())),
            )
            .unwrap();
        assert!(enriched.lexical_graph().is_empty());
        assert_eq!(
            enriched
                .interface_owners()
                .into_iter()
                .map(|entry| entry.owner)
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([
                identity("main", &owner.module_name()),
                identity("fixture", "Hidden"),
            ])
        );
        assert!(current
            .clone()
            .extend_checked_value_input_context(
                &initial,
                std::iter::once((owner, b"wrong".as_slice()))
            )
            .is_err());
        let foreign = current
            .extend_checked_original_products(
                [3; 32],
                &[support_product("Foreign")],
                &BTreeMap::new(),
            )
            .unwrap();
        assert!(foreign
            .extend_checked_value_input_context(
                &initial,
                std::iter::once((owner, evidence.interface_bytes())),
            )
            .is_err());
    }

    #[test]
    fn program_values_retain_private_type_owners_without_selecting_their_names() {
        let directory = tempfile::tempdir().unwrap();
        let baseline = Arc::new(
            ExactDeclarationContext::new(&[], &[], vec![])
                .unwrap()
                .extend_checked_original_products(
                    [2; 32],
                    &[support_product("Public")],
                    &BTreeMap::new(),
                )
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
                &[
                    support_product("InstanceOwner"),
                    support_product("InstanceRelay"),
                ],
                &[support_admission(directory.path())],
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
            .entries_for_owners(std::iter::once(identity("fixture", "ValFirst")));
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
                .extend_checked_original_products([2; 32], &[product], &BTreeMap::new())
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
            program_support: None,
            program_source_lexical: Vec::new(),
            source_selected_support: BTreeSet::new(),
            source_search_include: None,
            checked_value_imports: Default::default(),
            generated_scaffold_imports: None,
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
                .extend_checked_original_products([2; 32], &[product], &BTreeMap::new())
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
            program_support: None,
            program_source_lexical: Vec::new(),
            source_selected_support: BTreeSet::new(),
            source_search_include: None,
            checked_value_imports: Default::default(),
            generated_scaffold_imports: None,
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
            .extend_checked_original_products(
                [2; 32],
                std::slice::from_ref(&product),
                &BTreeMap::new(),
            )
            .unwrap();
        assert!(context.lexical_graph().is_empty());
        assert_eq!(context.artifact_view().descriptors().len(), 2);
        let repeated = context
            .clone()
            .extend_checked_original_products(
                [2; 32],
                std::slice::from_ref(&product),
                &BTreeMap::new(),
            )
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
            .extend_checked_original_products([2; 32], &[changed], &BTreeMap::new())
            .is_err());
        assert!(context
            .extend_checked_original_products([99; 32], &[product], &BTreeMap::new())
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
        Arc::new(ArtifactEntry::original([7; 32], product, vec![]).unwrap())
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
                    &BTreeMap::new(),
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
        unprotected.generated_scaffold_imports = None;
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

    #[test]
    fn generated_scaffold_authority_preserves_target_and_original_owner() {
        let directory = tempfile::tempdir().unwrap();
        let (graph, owners) = crate::execution_source::test_graph(directory.path());
        let inventory = ArtifactInventory::default();
        let context = ExactDeclarationContext {
            producer: [7; 32],
            inventory: inventory
                .admit_shared(
                    &inventory.empty_view(),
                    vec![execution_entry(owners[0].clone(), graph)],
                )
                .unwrap(),
            lexical: vec![],
        };
        let authority = GeneratedScaffoldImportAuthority {
            owner: owners[0].clone(),
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
        changed.owner.module_version = ModuleVersion([99; 32]);
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
        assert_eq!(decoded.as_array().unwrap()[1], text("8"));
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
                [2; 32],
            );
            Arc::new(ArtifactEntry::original([7; 32], product, vec![]).unwrap())
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
                .extend_checked_original_products(producer_sha256, &[product], &BTreeMap::new())
                .unwrap(),
        );
        let collision = root.join("collision");
        std::fs::create_dir_all(&collision).unwrap();
        let collision_graph = collision.join(format!("execution-{}.cbor", hex(&graph.digest())));
        std::fs::write(&collision_graph, b"prior immutable capture").unwrap();
        assert!(context.prepare_compilation(&collision, producer).is_err());
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
        assert_eq!(fields[1], text("8"));
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
            expected[1] = text("8");
            expected.push(Value::Null);
            expected.push(authorization.clone().unwrap_or(Value::Null));
            let mut legacy = Vec::new();
            ciborium::ser::into_writer(&Value::Array(expected), &mut legacy).unwrap();
            assert_eq!(
                encode_scope_manifest(base.clone(), None, authorization.clone()).unwrap(),
                legacy,
                "v8 always retains explicit execution and purpose fields"
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
            assert_eq!(decoded[1], text("8"));
            assert_eq!(decoded[8], authorization.unwrap_or(Value::Null));
        }
    }
}
