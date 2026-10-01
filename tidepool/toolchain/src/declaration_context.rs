//! Exact declaration inputs retain original products independently of source
//! lookup, while their explicit virtual graph owns lexical visibility.

use ciborium::value::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::artifact_inventory::{
    ArtifactEntry, ArtifactInventory, ArtifactKind, ArtifactPayload, ArtifactView,
};
use crate::certified_products::{
    certify_inherited_products_with_validation, InheritedProductInput, PendingCertifiedGroup,
};
use crate::declaration_join::{
    AcceptedJoin, CertifiedAuthoredDeclaration, DeclarationArtifact, ExactIfaceArtifact,
    ExactInterfaceOwner, ExactLexicalNode, ExactModuleIdentity, ModuleSnapshot,
};
use crate::recovery_artifacts::{
    self, CertifiedJoinedInterface, CertifiedRecoveryProduct, CertifiedValueInterface,
    PackageInterfaceValidation, RecoveryArtifactRef, RecoveryJoinRef, RecoveryValueInterfaceRef,
};
use crate::CompileError;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExactDeclarationContext {
    producer: [u8; 32],
    inventory: ArtifactView,
    lexical: Vec<ExactLexicalNode>,
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
            joins,
            values,
            Some((descriptors, interfaces)),
        )
    }

    fn capture_inputs(
        root: &Path,
        products: &[RecoveryArtifactRef],
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
        let mut verified = Vec::new();
        for reference in products {
            match recovery_artifacts::verify_materialized_ref_with_validation(
                root,
                reference,
                &mut validation,
            ) {
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
        let inputs = verified
            .iter()
            .map(|artifact| InheritedProductInput { artifact })
            .collect::<Vec<_>>();
        let requirements = crate::certified_products::certify_recovery_products_with_validation(
            &inputs,
            &mut validation,
        )
        .map_err(failure)?;
        let mut original_requirements = BTreeMap::new();
        for (artifact, requirements) in verified.into_iter().zip(requirements) {
            context.admit_producer(artifact.reference.toolchain_identity_sha256)?;
            let product = CertifiedRecoveryProduct::from_certification(
                tidepool_repr::execution_schema::CachedHomeOwner {
                    unit: artifact.reference.unit,
                    module: artifact.reference.module,
                    module_version: tidepool_repr::execution_schema::ModuleVersion(
                        artifact.reference.module_version,
                    ),
                    skinny_iface_sha256: artifact.reference.skinny_iface_sha256,
                    product_sha256: artifact.reference.product_sha256,
                },
                artifact.interface_bytes,
                artifact.product_bytes,
                artifact.package_imports_bytes,
                artifact.certification_bytes,
            );
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
        let owners = entries
            .iter()
            .map(|entry| (entry.descriptor.owner.clone(), entry))
            .collect::<BTreeMap<_, _>>();
        if owners.len() != entries.len() {
            return Err(failure("ambiguous recovered dependency owner"));
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
            .into_iter()
            .filter(|entry| !baseline_ids.contains(&entry.descriptor.id))
            .collect::<Vec<_>>();
        if !new_entries.is_empty() {
            std::fs::create_dir_all(root)?;
        }
        let mut validation = PackageInterfaceValidation::default();
        let (mut materialized, references) =
            context.materialize_entries_with_validation(root, &new_entries, &mut validation)?;
        materialized
            .artifacts
            .extend(self.artifacts.iter().cloned());
        let verified = references
            .iter()
            .map(|reference| {
                recovery_artifacts::verify_materialized_ref_with_validation(
                    root,
                    reference,
                    &mut validation,
                )
            })
            .collect::<Result<Vec<_>, _>>()
            .map_err(failure)?;
        let additional = certify_inherited_products_with_validation(
            &verified
                .iter()
                .map(|artifact| InheritedProductInput { artifact })
                .collect::<Vec<_>>(),
            &self.groups,
            &mut validation,
        )
        .map_err(failure)?;
        let groups = if additional.is_empty() {
            Arc::clone(&self.groups)
        } else {
            let mut groups = self.groups.to_vec();
            groups.extend(additional);
            groups.into()
        };
        Ok(Self {
            context,
            manifest: self.manifest.clone(),
            request_sha256: self.request_sha256.clone(),
            semantic_sha256: self.semantic_sha256,
            producer_sha256: self.producer_sha256,
            artifacts: materialized.artifacts,
            groups: groups.into(),
            program_support: self.program_support.clone(),
        })
    }

    pub(crate) fn admit_program_support(
        &mut self,
        context: Arc<ExactDeclarationContext>,
        products: &[CertifiedRecoveryProduct],
        admissions: &[ExactSourceAdmission],
    ) -> Result<Arc<ExactDeclarationContext>, CompileError> {
        let mut imports = BTreeMap::new();
        for admission in admissions {
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
                imports.contains_key(&identity(&product.owner().unit, &product.owner().module))
            })
            .collect::<Vec<_>>();
        for product in products {
            let owner = identity(&product.owner().unit, &product.owner().module);
            if imports.contains_key(&owner) {
                if retained.contains_key(&owner) {
                    return Err(failure(
                        "program support cannot select a retained hidden owner",
                    ));
                }
            } else if !retained.contains_key(&owner) {
                return Err(failure("program support lacks its fresh source admission"));
            }
        }
        let context = Arc::new((*context).clone().extend_checked_original_products(
            self.producer_sha256,
            products,
            &imports,
        )?);
        if fresh.is_empty() {
            return Ok(context);
        }
        let entries = context.artifact_view().entries_for_owners(
            fresh
                .iter()
                .map(|product| identity(&product.owner().unit, &product.owner().module)),
        );
        let fresh = context
            .artifact_view()
            .select_roots(entries.values().map(|entry| entry.descriptor.id).collect())?;
        self.program_support = Some(match &self.program_support {
            Some(previous) => previous.merge(&fresh)?,
            None => fresh,
        });
        Ok(context)
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
        planned: Option<&ExactModuleIdentity>,
    ) -> Result<Vec<ExactSourceAdmission>, CompileError> {
        self.validate_outputs_selected(root, planned, &self.context)
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
        self.context.validate_artifacts(&self.artifacts)?;
        if sha256(&std::fs::read(&self.manifest)?) != self.request_sha256 {
            return Err(failure("scope request changed during compilation"));
        }
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
        receipts
            .iter()
            .map(|path| self.validate_receipt(&path.join("receipt.cbor"), planned, context))
            .collect()
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
        let header = row(&value, 9)?;
        if string(&header[0])? != "TPEXACTCOMPILE"
            || string(&header[1])? != "1"
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
        selected.extend(support_entries.iter().map(|entry| {
            (
                entry.descriptor.owner.unit.as_str(),
                entry.descriptor.owner.module.as_str(),
            )
        }));
        if let Some(planned) = planned {
            selected.insert((planned.unit.as_str(), planned.module.as_str()));
        }
        let edges = list(&header[8], 4096)?;
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
                if boot
                    || !selected.contains(&(unit, name))
                    || (qualifier != "none" && qualifier != format!("this:{unit}"))
                    || !imported.insert((qualifier, name, boot, unit))
                {
                    return Err(failure(
                        "exact import witness leaves selected lexical graph",
                    ));
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
        joins: &[RecoveryJoinRef],
        lexical: Vec<ExactLexicalNode>,
    ) -> Result<Self, CompileError> {
        Self::capture_recovery_with_value_interfaces(root, products, joins, &[], lexical)
    }

    pub fn capture_recovery_with_value_interfaces(
        root: &Path,
        products: &[RecoveryArtifactRef],
        joins: &[RecoveryJoinRef],
        values: &[RecoveryValueInterfaceRef],
        lexical: Vec<ExactLexicalNode>,
    ) -> Result<Self, CompileError> {
        Self::capture_recovery_inputs(root, products, joins, values, None, lexical)
    }
    pub fn capture_recovery_with_inventory(
        root: &Path,
        products: &[RecoveryArtifactRef],
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
            joins,
            values,
            Some((descriptors, dependencies)),
            lexical,
        )
    }
    fn capture_recovery_inputs(
        root: &Path,
        products: &[RecoveryArtifactRef],
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
        let inventory =
            RecoveredArtifactInventory::capture_inputs(root, products, joins, values, inventory)
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
        for product in products {
            let owner = identity(&product.owner().unit, &product.owner().module);
            if let Some(entry) = existing.get(&owner) {
                let ArtifactPayload::Original(previous) = &entry.payload else {
                    return Err(failure(
                        "supporting original collides with type-only interface",
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
            let mut requirements = crate::certified_products::certified_home_requirements(
                product.certification_bytes(),
                product.owner(),
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
    pub(crate) fn interface_owners(&self) -> Vec<ExactInterfaceOwner> {
        self.inventory.interface_owners()
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
        use sha2::Digest;
        let mut lexical = self.lexical.iter().collect::<Vec<_>>();
        lexical.sort_by_key(|node| &node.owner);
        let value = Value::Array(vec![
            text("TPEXACTCONTEXT"),
            text("2"),
            text(hex(&sha2::Sha256::digest(
                serde_json::to_vec(&(self.inventory.descriptors(), self.inventory.dependencies()))
                    .expect("inventory encoding"),
            )
            .into())),
            text(hex(&self.producer)),
            Value::Array(
                self.recovery_products()
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
                self.joined_interfaces()
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

    pub(crate) fn validate_artifacts(
        &self,
        artifacts: &[DeclarationArtifact],
    ) -> Result<(), CompileError> {
        let interfaces = self.interface_owners();
        let products = self.recovery_products();
        let mut joins = self.joined_interfaces();
        joins.extend(
            self.value_interfaces()
                .into_iter()
                .map(|value| value.interface().clone()),
        );
        let mut seen = BTreeSet::new();
        for artifact in artifacts {
            let exact = &artifact.interface;
            let owner = identity(&exact.unit, &exact.module);
            if !seen.insert(owner.clone()) || !exact.path.is_absolute() {
                return Err(failure("duplicate owner or relative interface path"));
            }
            let metadata = interfaces
                .iter()
                .find(|entry| entry.owner == owner)
                .ok_or_else(|| failure("artifact is not owned by the context"))?;
            if exact.requirements.iter().cloned().collect::<BTreeSet<_>>()
                != metadata
                    .requirements
                    .iter()
                    .map(|owner| (owner.unit.clone(), owner.module.clone()))
                    .collect()
            {
                return Err(failure("artifact requirements differ from owned context"));
            }
            let (iface, packages) = if let Some(product) = products
                .iter()
                .find(|product| identity(&product.owner().unit, &product.owner().module) == owner)
            {
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
            } else {
                let join = joins
                    .iter()
                    .find(|join| identity(join.unit(), join.module()) == owner)
                    .ok_or_else(|| failure("synthetic anchor is missing"))?;
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
        if seen.len() != self.interface_owners().len() {
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
        self.materialize_with_validation(root, &mut PackageInterfaceValidation::default())
            .map(|(materialized, _)| materialized)
    }

    fn materialize_with_validation(
        &self,
        root: &Path,
        validation: &mut PackageInterfaceValidation,
    ) -> Result<
        (
            MaterializedExactDeclarationContext,
            Vec<RecoveryArtifactRef>,
        ),
        CompileError,
    > {
        self.materialize_entries_with_validation(root, &self.inventory.entries(), validation)
    }

    fn materialize_entries_with_validation(
        &self,
        root: &Path,
        entries: &[Arc<ArtifactEntry>],
        validation: &mut PackageInterfaceValidation,
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
            )
            .map_err(failure)?
        };
        let joined = entries
            .iter()
            .filter_map(|entry| match &entry.payload {
                ArtifactPayload::Interface(interface, _) => Some(interface),
                _ => None,
            })
            .map(|interface| interface.materialize_with_validation(root, validation))
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
        for reference in joined {
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

    fn materialized_groups(
        &self,
        root: &Path,
        references: &[RecoveryArtifactRef],
        validation: &mut PackageInterfaceValidation,
    ) -> Result<Vec<PendingCertifiedGroup>, CompileError> {
        let verified = references
            .iter()
            .map(|reference| {
                recovery_artifacts::verify_materialized_ref_with_validation(
                    root, reference, validation,
                )
            })
            .collect::<Result<Vec<_>, _>>()
            .map_err(failure)?;
        certify_inherited_products_with_validation(
            &verified
                .iter()
                .map(|artifact| InheritedProductInput { artifact })
                .collect::<Vec<_>>(),
            &[],
            validation,
        )
        .map_err(failure)
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
        use sha2::Digest;
        let admitted_empty = authorization.is_some()
            && self.producer == [0; 32]
            && self.recovery_products().is_empty()
            && self.joined_interfaces().is_empty()
            && self.interface_owners().is_empty()
            && self.lexical.is_empty();
        if !root.is_absolute()
            || (!admitted_empty
                && (self.producer == [0; 32]
                    || self.producer != <[u8; 32]>::from(sha2::Sha256::digest(producer))))
        {
            return Err(failure(
                "compile request has a different producer or invalid root",
            ));
        }
        std::fs::create_dir_all(root)?;
        // Package reads share one synchronous preparation snapshot. It does not
        // escape this stage; post-worker verification opens a fresh snapshot.
        let mut validation = PackageInterfaceValidation::default();
        let (materialized, references) = self.materialize_with_validation(root, &mut validation)?;
        self.validate_artifacts(&materialized.artifacts)?;
        let groups = self.materialized_groups(root, &references, &mut validation)?;
        let semantic_sha256 = self.semantic_sha256();
        let mut fields = vec![
            text("TPEXACTSCOPE"),
            text(if authorization.is_some() { "4" } else { "2" }),
            text(hex(&semantic_sha256)),
            text(sha256(producer)),
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
                self.recovery_products()
                    .iter()
                    .map(|product| {
                        let owner = product.owner();
                        let artifact = materialized
                            .artifacts
                            .iter()
                            .find(|artifact| {
                                artifact.interface.unit == owner.unit
                                    && artifact.interface.module == owner.module
                            })
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
        if let Some(authorization) = authorization {
            fields.push(authorization);
        }
        let value = Value::Array(fields);
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(&value, &mut bytes).map_err(failure)?;
        if bytes.len() > 4 * 1024 * 1024 {
            return Err(failure("scope manifest exceeds four MiB"));
        }
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
            producer_sha256: sha2::Sha256::digest(producer).into(),
            artifacts: materialized.artifacts,
            groups: groups.into(),
            program_support: None,
        })
    }
}

pub(crate) fn certified_artifact_view(
    producer: [u8; 32],
    products: &[CertifiedRecoveryProduct],
    interfaces: &[ExactInterfaceOwner],
    joined: &[CertifiedJoinedInterface],
    baseline: Option<&ExactDeclarationContext>,
) -> Result<ArtifactView, CompileError> {
    let view = baseline.map_or_else(
        || ArtifactInventory::default().empty_view(),
        |context| context.artifact_view().clone(),
    );
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
    use sha2::{Digest, Sha256};
    use tidepool_repr::execution_schema::{CachedHomeOwner, ModuleVersion};

    fn support_product(module: &str) -> CertifiedRecoveryProduct {
        let interface = format!("{module} interface").into_bytes();
        let mut product = Vec::new();
        ciborium::ser::into_writer(
            &Value::Array(vec![
                text("TPMOD"),
                Value::Integer(1.into()),
                Value::Array(vec![Value::Array(vec![
                    text("fixture"),
                    text(module),
                    Value::Bytes(interface.clone()),
                    Value::Array(vec![]),
                ])]),
            ]),
            &mut product,
        )
        .unwrap();
        let owner = CachedHomeOwner {
            unit: "fixture".into(),
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
                text("1"),
                Value::Array(vec![
                    text(&owner.unit),
                    text(&owner.module),
                    text(sha256(&interface)),
                ]),
                Value::Array(vec![]),
            ]),
            &mut packages,
        )
        .unwrap();
        CertifiedRecoveryProduct::from_certification(
            owner,
            interface,
            product,
            packages,
            certification,
        )
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
        }
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
        }
    }

    fn import_receipt(root: &Path, request: &ExactCompilationRequest, imported: &str) -> PathBuf {
        let directory = root.join(format!("receipt-{imported}"));
        std::fs::create_dir_all(&directory).unwrap();
        let source = "module Consumer where\n";
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
            text("1"),
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
                    text("none"),
                    text(imported),
                    Value::Bool(false),
                    text("fixture"),
                ])]),
            ])]),
        ]);
        let receipt = directory.join("receipt.cbor");
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(&value, &mut bytes).unwrap();
        std::fs::write(&receipt, bytes).unwrap();
        receipt
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
            text("1"),
            Value::Array(vec![
                text(&owner.unit),
                text(&owner.module),
                text(sha256(&interface)),
            ]),
            Value::Array(vec![]),
        ]);
        let mut package_bytes = Vec::new();
        ciborium::ser::into_writer(&package_value, &mut package_bytes).unwrap();
        let product = CertifiedRecoveryProduct::from_certification(
            owner,
            interface,
            product_bytes,
            package_bytes,
            certification,
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
        let product = CertifiedRecoveryProduct::from_certification(
            owner,
            b"interface".to_vec(),
            b"product".to_vec(),
            Vec::new(),
            certification,
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
        let product = CertifiedRecoveryProduct::from_certification(
            owner.clone(),
            b"interface".to_vec(),
            b"product".to_vec(),
            Vec::new(),
            certification.clone(),
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
        assert_eq!(context.artifact_view().descriptors().len(), 1);
        let repeated = context
            .clone()
            .extend_checked_original_products(
                [2; 32],
                std::slice::from_ref(&product),
                &BTreeMap::new(),
            )
            .unwrap();
        assert_eq!(context.semantic_sha256(), repeated.semantic_sha256());
        assert_eq!(repeated.artifact_view().inventory().node_count(), 1);
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
}
