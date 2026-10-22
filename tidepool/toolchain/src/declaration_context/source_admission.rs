//! Source visibility is issued from authenticated receipts, separately from
//! the compiler dependency graph and protected template imports.

use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceAdmissionBoundary {
    FiniteRequest,
    WorldStage(usize),
    AuthoredDeclaration,
    OriginalExecution,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceLexicalClaim {
    FreshSource,
    CurrentSelectedOriginal,
    PriorProgramSource,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceAdmissionRefusal {
    WrongRequest,
    ProducedTypeClaim,
    HiddenOriginalPromotion,
    ChangedCanonical,
    MissingCanonical,
    ChangedSynthetic,
    ConflictingClaim,
}

#[derive(Debug)]
pub struct SourceAdmissionFailure {
    pub owner: ExactModuleIdentity,
    pub boundary: SourceAdmissionBoundary,
    pub claim: Option<SourceLexicalClaim>,
    pub held: Option<crate::artifact_inventory::ArtifactKind>,
    pub reason: SourceAdmissionRefusal,
}

impl std::fmt::Display for SourceAdmissionFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "source admission {:?}: owner={}:{} boundary={:?} claim={:?} held={:?}",
            self.reason, self.owner.unit, self.owner.module, self.boundary, self.claim, self.held
        )
    }
}

impl std::error::Error for SourceAdmissionFailure {}

/// Consumed compiler adjacency carries no lexical permission. Its construction
/// is private to an already authenticated source receipt.
#[derive(Clone, Debug)]
pub(crate) struct CompilerDependencies {
    adjacency: BTreeMap<ExactModuleIdentity, Vec<ExactModuleIdentity>>,
}

impl CompilerDependencies {
    pub(super) fn merge<'a>(
        admissions: impl IntoIterator<Item = &'a ExactSourceAdmission>,
    ) -> Result<Self, CompileError> {
        let mut result = Self {
            adjacency: BTreeMap::new(),
        };
        for admission in admissions {
            for (owner, edges) in admission.compiler_dependencies().adjacency() {
                charge_owner(&admission.claims.operation, owner, 1)?;
                for child in edges {
                    charge_owner(&admission.claims.operation, child, 1)?;
                }
                if result
                    .adjacency
                    .insert(owner.clone(), edges.clone())
                    .is_some_and(|previous| previous != *edges)
                {
                    return Err(failure("source admission changed consumed adjacency"));
                }
            }
        }
        Ok(result)
    }
    pub(crate) fn adjacency(&self) -> &BTreeMap<ExactModuleIdentity, Vec<ExactModuleIdentity>> {
        &self.adjacency
    }

    pub(crate) fn owners(&self) -> BTreeSet<ExactModuleIdentity> {
        self.adjacency.keys().cloned().collect()
    }
}

/// One resolved direct HOME edge from the authenticated root. Decoding this
/// row checks syntax only; it grants no import or lexical authority.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct RawSourceImportRole {
    pub(super) qualifier: String,
    pub(super) module: String,
    pub(super) boot: bool,
    pub(super) unit: String,
    pub(super) mask: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RootImportRole {
    AuthoredCurrent,
    InheritedLexical,
    ProtectedCompiler,
}

/// Issued only after root authentication and exact direct HOME-edge coverage.
/// The role census classifies source use; exact import permission is separate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RootSourceImportRoles {
    root: ExactModuleIdentity,
    rows: Vec<RawSourceImportRole>,
    masks: BTreeMap<ExactModuleIdentity, u8>,
    inherited_lexical: BTreeSet<ExactModuleIdentity>,
}

impl RootSourceImportRoles {
    pub(super) fn issue(
        evidence: &crate::cache::CompletedSourceEvidence,
        source_path: &Path,
        exact: &[ExactReceiptModule],
        rows: Vec<RawSourceImportRole>,
        prior_lexical: impl Fn(&ExactModuleIdentity) -> bool,
        operation: &tidepool_repr::execution_schema::InventoryOperation,
    ) -> Result<Arc<Self>, CompileError> {
        let root = authenticated_compilation_root(evidence, source_path)?;
        let mut expected = BTreeSet::new();
        let mut insert = |qualifier: &crate::cache::ImportQualifier,
                          module: &str,
                          boot: bool,
                          unit: &str| {
            let qualifier_bytes = match qualifier {
                crate::cache::ImportQualifier::Unqualified => 4,
                crate::cache::ImportQualifier::ThisUnit(unit) => 5usize.saturating_add(unit.len()),
                crate::cache::ImportQualifier::OtherUnit(unit) => 6usize.saturating_add(unit.len()),
            };
            operation
                .reserve::<(String, String, bool, String)>(1)
                .and_then(|_| {
                    operation.charge(
                        qualifier_bytes
                            .saturating_add(module.len())
                            .saturating_add(unit.len()),
                    )
                })
                .map_err(|error| compiler_evidence_failure(error.into()))?;
            let qualifier = String::from(qualifier.clone());
            expected.insert((qualifier, module.to_owned(), boot, unit.to_owned()));
            Ok::<_, CompileError>(())
        };
        let node = evidence
            .modules
            .iter()
            .find(|node| !node.boot && node.unit == root.unit && node.module == root.module)
            .ok_or_else(|| failure("source roles lack their authenticated root"))?;
        for edge in &node.imports {
            if edge.selected.is_some() {
                insert(&edge.qualifier, &edge.module, edge.boot, &root.unit)?;
            }
        }
        for module in exact
            .iter()
            .filter(|module| !module.boot && module.owner == root)
        {
            for edge in &module.imports {
                insert(&edge.qualifier, &edge.module, edge.boot, &edge.unit)?;
            }
        }
        if !expected
            .iter()
            .map(|(q, m, b, u)| (q.as_str(), m.as_str(), *b, u.as_str()))
            .eq(rows.iter().map(|row| {
                (
                    row.qualifier.as_str(),
                    row.module.as_str(),
                    row.boot,
                    row.unit.as_str(),
                )
            }))
        {
            return Err(failure(
                "source import roles differ from authenticated root HOME edges",
            ));
        }
        let mut masks = BTreeMap::new();
        for row in &rows {
            charge_owner_fields(operation, &row.unit, &row.module, 1)?;
            let imported = identity(&row.unit, &row.module);
            *masks.entry(imported).or_insert(0) |= row.mask;
        }
        let mut inherited_lexical = BTreeSet::new();
        for (imported, mask) in &masks {
            if mask & 1 == 0 && mask & 2 != 0 && prior_lexical(imported) {
                charge_owner(operation, imported, 1)?;
                inherited_lexical.insert(imported.clone());
            }
        }
        let owner = Self {
            root,
            rows,
            masks,
            inherited_lexical,
        };
        Ok(Arc::new(owner))
    }

    fn role(&self, source: &ExactModuleIdentity, imported: &ExactModuleIdentity) -> RootImportRole {
        if source != &self.root {
            return RootImportRole::AuthoredCurrent;
        }
        let mask = self.masks.get(imported).copied().unwrap_or(0);
        if mask & 1 != 0 {
            RootImportRole::AuthoredCurrent
        } else if mask & 2 != 0 {
            RootImportRole::InheritedLexical
        } else {
            RootImportRole::ProtectedCompiler
        }
    }

    #[cfg(test)]
    pub(super) fn observed_rows(&self) -> impl Iterator<Item = (&str, &str, bool, &str, u8, bool)> {
        self.rows.iter().map(|row| {
            let imported = identity(&row.unit, &row.module);
            (
                row.qualifier.as_str(),
                row.module.as_str(),
                row.boot,
                row.unit.as_str(),
                row.mask,
                self.admits_lexical_edge(&self.root, &imported),
            )
        })
    }

    pub(crate) fn compilation_root(&self) -> &ExactModuleIdentity {
        &self.root
    }

    fn admits_lexical_edge(
        &self,
        source: &ExactModuleIdentity,
        imported: &ExactModuleIdentity,
    ) -> bool {
        match self.role(source, imported) {
            RootImportRole::AuthoredCurrent => true,
            RootImportRole::InheritedLexical => self.inherited_lexical.contains(imported),
            RootImportRole::ProtectedCompiler => false,
        }
    }
}

/// Pre-product claims have already passed source/path/import authentication.
/// Only the product-bound issuer below can turn them into source admissions.
#[derive(Clone)]
pub(super) struct ReceiptSourceClaims {
    operation: Arc<tidepool_repr::execution_schema::InventoryOperation>,
    dependencies: CompilerDependencies,
    lexical: BTreeMap<ExactModuleIdentity, SourceLexicalClaim>,
    pub(super) roles: Arc<RootSourceImportRoles>,
}

impl ReceiptSourceClaims {
    pub(super) fn issue(
        evidence: &crate::cache::CompletedSourceEvidence,
        exact_imports: &BTreeMap<ExactModuleIdentity, Vec<ExactModuleIdentity>>,
        selected: &BTreeMap<ExactModuleIdentity, crate::execution_source::SourceSelectedOriginal>,
        roles: Arc<RootSourceImportRoles>,
        prior: impl Fn(&ExactModuleIdentity) -> bool,
        operation: &Arc<tidepool_repr::execution_schema::InventoryOperation>,
    ) -> Result<Self, CompileError> {
        // Bound the new owned maps and traversal before allocating them. Source
        // bytes and decoded receipt facts were charged by their existing owner.
        for module in evidence.modules.iter().filter(|node| !node.boot) {
            operation
                .reserve::<(ExactModuleIdentity, Vec<ExactModuleIdentity>)>(3)
                .and_then(|_| {
                    operation.charge(
                        module
                            .unit
                            .len()
                            .saturating_add(module.module.len())
                            .saturating_mul(3),
                    )
                })
                .map_err(|error| compiler_evidence_failure(error.into()))?;
            for edge in &module.imports {
                operation
                    .reserve::<ExactModuleIdentity>(2)
                    .and_then(|_| {
                        operation.charge(
                            module
                                .unit
                                .len()
                                .saturating_add(edge.module.len())
                                .saturating_mul(2),
                        )
                    })
                    .map_err(|error| compiler_evidence_failure(error.into()))?;
            }
        }
        for (owner, imports) in exact_imports {
            charge_owner(operation, owner, 1)?;
            for child in imports {
                charge_owner(operation, child, 1)?;
            }
        }
        for (owner, original) in selected {
            charge_owner(operation, owner, 3)?;
            for child in original.imports() {
                charge_owner(operation, child, 1)?;
            }
        }
        let mut adjacency = consumed_source_home_imports(evidence, exact_imports)?;
        for (owner, original) in selected {
            if adjacency
                .insert(owner.clone(), original.imports().to_vec())
                .is_some()
            {
                return Err(failure(
                    "fresh source collides with a source-selected original",
                ));
            }
        }
        let compilation_root = roles.compilation_root();
        let mut lexical = BTreeMap::new();
        let mut pending = vec![compilation_root.clone()];
        while let Some(owner) = pending.pop() {
            crate::host_work::checkpoint()?;
            if lexical.contains_key(&owner) {
                continue;
            }
            let Some(imports) = adjacency.get(&owner) else {
                continue;
            };
            let kind = if selected.contains_key(&owner) {
                SourceLexicalClaim::CurrentSelectedOriginal
            } else if prior(&owner) {
                SourceLexicalClaim::PriorProgramSource
            } else {
                SourceLexicalClaim::FreshSource
            };
            lexical.insert(owner.clone(), kind);
            pending.extend(
                imports
                    .iter()
                    // Inherited links borrow the earlier Source scope. Its
                    // compiler adjacency cannot issue new source claims.
                    .filter(|child| roles.role(&owner, child) == RootImportRole::AuthoredCurrent)
                    .cloned(),
            );
        }
        Ok(Self {
            operation: Arc::clone(operation),
            dependencies: CompilerDependencies { adjacency },
            lexical,
            roles,
        })
    }

    pub(super) fn compilation_root(&self) -> &ExactModuleIdentity {
        self.roles.compilation_root()
    }

    pub(super) fn lexical_claims(
        &self,
    ) -> impl Iterator<Item = (&ExactModuleIdentity, SourceLexicalClaim)> {
        self.lexical.iter().map(|(owner, claim)| (owner, *claim))
    }

    pub(super) fn lexical_imports(
        &self,
        owner: &ExactModuleIdentity,
    ) -> Option<Vec<ExactModuleIdentity>> {
        self.lexical
            .get(owner)
            .and_then(|_| self.dependencies.adjacency.get(owner))
            .map(|imports| {
                imports
                    .iter()
                    .filter(|child| self.roles.admits_lexical_edge(owner, child))
                    .cloned()
                    .collect()
            })
    }

    pub(super) fn dependencies(&self) -> &CompilerDependencies {
        &self.dependencies
    }
}

pub(super) fn authenticated_compilation_root(
    evidence: &crate::cache::CompletedSourceEvidence,
    source_path: &Path,
) -> Result<ExactModuleIdentity, CompileError> {
    let mut owners = evidence.modules.iter().filter(|node| {
        !node.boot
            && (node.source == Path::new(crate::cache::GENERATED_SOURCE)
                || node.source == source_path)
    });
    match (owners.next(), owners.next()) {
        (Some(owner), None) => Ok(identity(&owner.unit, &owner.module)),
        _ => Err(failure(
            "source claims lack one authenticated compilation root",
        )),
    }
}

fn charge_owner(
    operation: &tidepool_repr::execution_schema::InventoryOperation,
    owner: &ExactModuleIdentity,
    copies: usize,
) -> Result<(), CompileError> {
    charge_owner_fields(operation, &owner.unit, &owner.module, copies)
}

fn charge_owner_fields(
    operation: &tidepool_repr::execution_schema::InventoryOperation,
    unit: &str,
    module: &str,
    copies: usize,
) -> Result<(), CompileError> {
    operation
        .reserve::<(ExactModuleIdentity, Vec<ExactModuleIdentity>)>(copies)
        .and_then(|_| {
            operation.charge(
                unit.len()
                    .saturating_add(module.len())
                    .saturating_mul(copies),
            )
        })
        .map_err(|error| compiler_evidence_failure(error.into()))
}

#[derive(Clone, Debug, Default)]
struct LexicalAdmissions {
    claims: BTreeMap<ExactModuleIdentity, SourceLexicalClaim>,
}

/// The one product-bound source owner. Dependency custody, lexical claims and
/// protected links cannot be paired independently by its consumers.
#[derive(Clone)]
pub(crate) struct SourceAdmissionBundle {
    compiler: Arc<crate::artifact_inventory::CompilerSourceSupport>,
    dependencies: CompilerDependencies,
    lexical: LexicalAdmissions,
    roles: BTreeMap<ExactModuleIdentity, Arc<RootSourceImportRoles>>,
}

impl std::fmt::Debug for SourceAdmissionBundle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SourceAdmissionBundle")
            .field("dependencies", &self.dependencies.adjacency.len())
            .field("claims", &self.lexical.claims.len())
            .field("source_role_owners", &self.roles.len())
            .finish()
    }
}

impl SourceAdmissionBundle {
    pub(super) fn check_finite_request(
        expected: &ExactRequestIdentity<'_>,
        admissions: &[ExactSourceAdmission],
    ) -> Result<(), CompileError> {
        for admission in admissions {
            if !matches!(&admission.compilation_binding, SourceCompilationBinding::Exact(actual) if actual == expected)
            {
                let error = SourceAdmissionFailure {
                    owner: admission.claims.compilation_root().clone(),
                    boundary: SourceAdmissionBoundary::FiniteRequest,
                    claim: None,
                    held: None,
                    reason: SourceAdmissionRefusal::WrongRequest,
                };
                tracing::warn!(owner = ?error.owner, boundary = ?error.boundary, reason = ?error.reason, "source admission refused");
                return Err(CompileError::SourceAdmission(Box::new(error)));
            }
        }
        Ok(())
    }

    pub(super) fn issue<'a>(
        boundary: SourceAdmissionBoundary,
        compiler: Arc<crate::artifact_inventory::CompilerSourceSupport>,
        dependencies: CompilerDependencies,
        admissions: impl IntoIterator<Item = &'a ExactSourceAdmission>,
        supplied: impl IntoIterator<Item = (&'a ExactModuleIdentity, &'a Arc<ArtifactEntry>)>,
        retained: impl Fn(&ExactModuleIdentity) -> Option<&'a Arc<ArtifactEntry>>,
        source_input: impl Fn(&ExactModuleIdentity) -> Option<&'a Arc<ArtifactEntry>>,
        produced: Option<&crate::checked_cell::ProducedValueTypeInterfaces>,
    ) -> Result<Arc<Self>, CompileError> {
        let mut bundle = Self {
            compiler,
            dependencies,
            lexical: Default::default(),
            roles: Default::default(),
        };
        let mut selected = BTreeMap::new();
        for admission in admissions {
            for (owner, kind) in &admission.claims.lexical {
                charge_owner(&admission.claims.operation, owner, 2)?;
                if bundle
                    .lexical
                    .claims
                    .insert(owner.clone(), *kind)
                    .is_some_and(|previous| previous != *kind)
                {
                    return Err(failure("source admission changed its lexical claim"));
                }
            }
            let roles = &admission.claims.roles;
            charge_owner(&admission.claims.operation, &roles.root, 1)?;
            if bundle
                .roles
                .insert(roles.root.clone(), Arc::clone(roles))
                .is_some_and(|previous| previous != *roles)
            {
                return Err(failure("source admission changed its root import roles"));
            }
            for (owner, original) in &admission.selected_originals {
                if selected
                    .insert(owner.clone(), original)
                    .is_some_and(|previous| previous.interface() != original.interface())
                {
                    return Err(failure("source admission changed a selected original"));
                }
            }
        }
        let mut admitted = BTreeMap::new();
        for (owner, entry) in supplied {
            let claim = bundle.lexical.claims.get(owner).copied();
            let prior = retained(owner);
            let held = source_input(owner);
            let refuse = |reason| {
                let error = SourceAdmissionFailure {
                    owner: owner.clone(),
                    boundary,
                    claim,
                    held: held.map(|entry| entry.descriptor.kind),
                    reason,
                };
                tracing::warn!(?owner, ?boundary, ?claim, held = ?error.held, ?reason, "source admission refused");
                CompileError::SourceAdmission(Box::new(error))
            };
            if produced.is_some_and(|types| types.matches_artifact(entry)) {
                if claim.is_some() {
                    return Err(refuse(SourceAdmissionRefusal::ProducedTypeClaim));
                }
                continue;
            }
            match claim {
                Some(SourceLexicalClaim::CurrentSelectedOriginal) => {
                    let selected = selected
                        .get(owner)
                        .ok_or_else(|| refuse(SourceAdmissionRefusal::ConflictingClaim))?;
                    if prior.and_then(|entry| canonical_source_interface(entry))
                        != Some(selected.interface())
                        || canonical_source_interface(entry) != Some(selected.interface())
                    {
                        return Err(refuse(SourceAdmissionRefusal::ChangedCanonical));
                    }
                }
                Some(SourceLexicalClaim::FreshSource) => {
                    if held
                        .is_some_and(|entry| matches!(&entry.payload, ArtifactPayload::Original(_)))
                    {
                        return Err(refuse(SourceAdmissionRefusal::HiddenOriginalPromotion));
                    }
                    if prior.is_some_and(|previous| {
                        canonical_source_interface(previous) != canonical_source_interface(entry)
                    }) {
                        return Err(refuse(SourceAdmissionRefusal::ChangedCanonical));
                    }
                    if canonical_source_interface(entry).is_none() {
                        return Err(refuse(SourceAdmissionRefusal::MissingCanonical));
                    }
                }
                Some(SourceLexicalClaim::PriorProgramSource) => {
                    if prior.and_then(|entry| canonical_source_interface(entry))
                        != canonical_source_interface(entry)
                        || canonical_source_interface(entry).is_none()
                    {
                        return Err(refuse(SourceAdmissionRefusal::ChangedCanonical));
                    }
                }
                None => {
                    if canonical_source_interface(entry).is_none()
                        && prior
                            .is_none_or(|previous| previous.descriptor.id != entry.descriptor.id)
                    {
                        return Err(refuse(SourceAdmissionRefusal::ChangedSynthetic));
                    }
                }
            }
            if let Some(claim) = claim {
                admitted.insert(owner.clone(), claim);
            }
        }
        bundle.lexical.claims = admitted;
        tracing::debug!(
            ?boundary,
            dependencies = bundle.dependencies.adjacency.len(),
            claims = bundle.lexical.claims.len(),
            source_role_owners = bundle.roles.len(),
            "source admission issued"
        );
        Ok(Arc::new(bundle))
    }

    pub(crate) fn dependencies(&self) -> &CompilerDependencies {
        &self.dependencies
    }
    pub(super) fn compiler(&self) -> &crate::artifact_inventory::CompilerSourceSupport {
        &self.compiler
    }
    pub(super) fn lexical_roots(&self) -> impl Iterator<Item = &ExactModuleIdentity> {
        self.lexical.claims.keys()
    }
    pub(super) fn selected_roots(&self) -> impl Iterator<Item = &ExactModuleIdentity> {
        self.lexical.claims.iter().filter_map(|(owner, kind)| {
            (*kind == SourceLexicalClaim::CurrentSelectedOriginal).then_some(owner)
        })
    }
    pub(super) fn admits_lexical(&self, owner: &ExactModuleIdentity) -> bool {
        self.lexical.claims.contains_key(owner)
    }
    pub(super) fn lexical_imports(
        &self,
        owner: &ExactModuleIdentity,
    ) -> Option<Vec<ExactModuleIdentity>> {
        self.lexical
            .claims
            .contains_key(owner)
            .then(|| self.dependencies.adjacency.get(owner))
            .flatten()
            .map(|imports| {
                imports
                    .iter()
                    .filter(|child| {
                        self.roles
                            .get(owner)
                            .is_none_or(|roles| roles.admits_lexical_edge(owner, child))
                    })
                    .cloned()
                    .collect()
            })
    }
}

#[cfg(test)]
mod selection_tests {
    use super::*;
    use crate::cache::{
        DependencyEvidence, ImportQualifier, ModuleEvidence, ModuleImportEvidence,
        ProductAvailability, ResolutionEvidence, SourceEvidence,
    };
    use crate::execution_source::{
        validate_source_selected_originals, SourceSelectedOriginalClaim, SourceSelectionContext,
    };
    use sha2::{Digest, Sha256};

    #[test]
    fn canonical_source_selection_needs_no_native_recipe_and_refuses_drift() {
        let root = tempfile::tempdir().unwrap();
        let owner = ExactModuleIdentity {
            unit: "main".into(),
            module: "A".into(),
        };
        let consumer = ExactModuleIdentity {
            unit: "main".into(),
            module: "Consumer".into(),
        };
        let source = root.path().join("A.hs");
        let bytes = b"module A where\ntype Answer = Int\n";
        std::fs::write(&source, bytes).unwrap();
        let source_sha: [u8; 32] = Sha256::digest(bytes).into();
        let interface = crate::certified_products::fixture_source_module_interface(
            [2; 32],
            "main",
            "A",
            source_sha,
            BTreeMap::new(),
            None,
        );
        let exact = BTreeMap::from([(owner.clone(), interface.interface_sha256())]);
        let interfaces = [interface.clone()];
        let include = [root.path().to_path_buf()];
        let fresh = DependencyEvidence {
            version: 4,
            cache_safe: true,
            selection_complete: true,
            sources: vec![],
            resolutions: vec![],
            packages: vec![],
            modules: vec![ModuleEvidence {
                unit: "main".into(),
                module: "Consumer".into(),
                boot: false,
                source: root.path().join("Consumer.hs"),
                imports: vec![],
                product: ProductAvailability::Ready,
            }],
        };
        let evidence = DependencyEvidence {
            version: 4,
            cache_safe: true,
            selection_complete: true,
            sources: vec![SourceEvidence {
                path: source.clone(),
                sha256: hex(&source_sha),
            }],
            modules: vec![ModuleEvidence {
                unit: "main".into(),
                module: "A".into(),
                boot: false,
                source: source.clone(),
                imports: vec![],
                product: ProductAvailability::InterfaceOnly,
            }],
            packages: vec![],
            resolutions: vec![ResolutionEvidence {
                qualifier: ImportQualifier::Unqualified,
                module: "A".into(),
                boot: false,
                selected: Some(source.clone()),
                candidates: vec![source.clone()],
            }],
        };
        let roots = [(consumer, ImportQualifier::Unqualified, owner.clone())];
        let independent = BTreeSet::new();
        let source_less = BTreeSet::new();
        // Enter the private production issuer with an independently authenticated
        // root; the selection validator borrows the exact same checked owner.
        let role_path = root.path().join("Consumer.hs");
        let role_source = "module Consumer where\n";
        std::fs::write(&role_path, role_source).unwrap();
        let role_evidence = crate::cache::CompletedSourceEvidence::from_worker_evidence(
            DependencyEvidence {
                sources: vec![SourceEvidence {
                    path: role_path.clone(),
                    sha256: hex(&Sha256::digest(role_source.as_bytes()).into()),
                }],
                modules: vec![ModuleEvidence {
                    source: role_path.clone(),
                    ..fresh.modules[0].clone()
                }],
                version: 4,
                cache_safe: true,
                selection_complete: true,
                resolutions: vec![],
                packages: vec![],
            },
            &role_path,
            role_source,
        )
        .unwrap();
        let role_edges = vec![ExactReceiptModule {
            owner: roots[0].0.clone(),
            boot: false,
            imports: vec![ExactReceiptImport {
                qualifier: ImportQualifier::Unqualified,
                module: "A".into(),
                boot: false,
                unit: "main".into(),
            }],
        }];
        let role_row = RawSourceImportRole {
            qualifier: "none".into(),
            module: "A".into(),
            boot: false,
            unit: "main".into(),
            mask: 1,
        };
        let operation =
            tidepool_repr::execution_schema::InventoryOperation::new(Default::default());
        let import_roles = RootSourceImportRoles::issue(
            &role_evidence,
            &role_path,
            &role_edges,
            vec![role_row.clone()],
            |_| false,
            &operation,
        )
        .unwrap();
        let context = || SourceSelectionContext {
            producer: [2; 32],
            interfaces: &interfaces,
            exact_interfaces: &exact,
            include: &include,
            fresh: &fresh,
            roots: &roots,
            import_roles: &import_roles,
            independent: &independent,
            source_less: &source_less,
        };
        let claim = || SourceSelectedOriginalClaim {
            owner: owner.clone(),
            source_sha256: source_sha,
            certificate_sha256: Sha256::digest(interface.certificate_bytes()).into(),
            interface_sha256: interface.interface_sha256(),
        };
        assert!(validate_source_selected_originals(vec![claim()], &evidence, context()).is_ok());
        for mask in [3, 5, 7] {
            let overlapping = RootSourceImportRoles::issue(
                &role_evidence,
                &role_path,
                &role_edges,
                vec![RawSourceImportRole {
                    mask,
                    ..role_row.clone()
                }],
                |_| false,
                &operation,
            )
            .unwrap();
            assert!(validate_source_selected_originals(
                vec![claim()],
                &evidence,
                SourceSelectionContext {
                    import_roles: &overlapping,
                    ..context()
                }
            )
            .is_ok());
        }
        // Canonical qualification follows actual compiler consumption even when
        // the same exact edge grants no lexical source visibility.
        let protected_roles = RootSourceImportRoles::issue(
            &role_evidence,
            &role_path,
            &role_edges,
            vec![RawSourceImportRole {
                mask: 4,
                ..role_row
            }],
            |_| false,
            &operation,
        )
        .unwrap();
        let protected_context = || SourceSelectionContext {
            import_roles: &protected_roles,
            ..context()
        };
        assert!(
            validate_source_selected_originals(vec![claim()], &evidence, protected_context())
                .is_ok()
        );
        let mut malformed = claim();
        malformed.certificate_sha256[0] ^= 1;
        assert!(validate_source_selected_originals(
            vec![malformed],
            &evidence,
            protected_context()
        )
        .is_err());
        assert!(validate_source_selected_originals(vec![claim()], &evidence, context()).is_ok());
        // An adjacent consumed module is not another compilation root.
        let mut adjacent = fresh.clone();
        let mut unused = adjacent.modules[0].clone();
        unused.module = "Unused".into();
        unused.source = root.path().join("Unused.hs");
        adjacent.modules.push(unused);
        let unrooted = [(
            ExactModuleIdentity {
                unit: "main".into(),
                module: "Unused".into(),
            },
            ImportQualifier::Unqualified,
            owner.clone(),
        )];
        let refused = validate_source_selected_originals(
            vec![claim()],
            &evidence,
            SourceSelectionContext {
                fresh: &adjacent,
                roots: &unrooted,
                ..context()
            },
        )
        .unwrap_err();
        assert!(refused
            .to_string()
            .contains("selected owners are unreachable from actual imports"));

        // A duplicated alias/preamble edge retains the same actual source demand.
        let duplicate = [roots[0].clone(), roots[0].clone()];
        assert!(validate_source_selected_originals(
            vec![claim()],
            &evidence,
            SourceSelectionContext {
                roots: &duplicate,
                ..context()
            }
        )
        .is_ok());
        for field in 0..3 {
            let mut forged = claim();
            match field {
                0 => forged.certificate_sha256[0] ^= 1,
                1 => forged.interface_sha256[0] ^= 1,
                _ => forged.source_sha256[0] ^= 1,
            };
            assert!(
                validate_source_selected_originals(vec![forged], &evidence, context()).is_err()
            );
        }
        assert!(validate_source_selected_originals(
            vec![claim()],
            &evidence,
            SourceSelectionContext {
                producer: [9; 32],
                ..context()
            }
        )
        .is_err());
        assert!(validate_source_selected_originals(
            vec![claim()],
            &evidence,
            SourceSelectionContext {
                roots: &[],
                ..context()
            }
        )
        .is_err());
        std::fs::write(&source, b"module A where\ntype Answer = Bool\n").unwrap();
        assert!(validate_source_selected_originals(vec![claim()], &evidence, context()).is_err());
        std::fs::remove_file(&source).unwrap();
        assert!(validate_source_selected_originals(vec![claim()], &evidence, context()).is_err());
        std::fs::write(&source, bytes).unwrap();
        let earlier = root.path().join("earlier");
        std::fs::create_dir(&earlier).unwrap();
        let include_earlier = [earlier.clone(), root.path().to_path_buf()];
        assert!(validate_source_selected_originals(
            vec![claim()],
            &evidence,
            SourceSelectionContext {
                include: &include_earlier,
                ..context()
            }
        )
        .is_err());
        let alias_root = root.path().join("active");
        std::os::unix::fs::symlink(root.path(), &alias_root).unwrap();
        let alias_source = alias_root.join("A.hs");
        let mut alias_evidence = evidence.clone();
        alias_evidence.sources[0].path = alias_source.clone();
        alias_evidence.modules[0].source = alias_source.clone();
        alias_evidence.resolutions[0].selected = Some(alias_source.clone());
        alias_evidence.resolutions[0].candidates = vec![alias_source];
        let alias_include = [alias_root];
        assert!(validate_source_selected_originals(
            vec![claim()],
            &alias_evidence,
            SourceSelectionContext {
                include: &alias_include,
                ..context()
            }
        )
        .is_ok());
        let package = root.path().join("package.hi");
        std::fs::write(&package, b"canonical package interface").unwrap();
        let packaged = crate::certified_products::fixture_source_module_interface(
            [2; 32],
            "main",
            "A",
            source_sha,
            BTreeMap::new(),
            Some(&package),
        );
        let package_claim = || SourceSelectedOriginalClaim {
            owner: owner.clone(),
            source_sha256: source_sha,
            certificate_sha256: Sha256::digest(packaged.certificate_bytes()).into(),
            interface_sha256: packaged.interface_sha256(),
        };
        let packaged_interfaces = [packaged.clone()];
        assert!(validate_source_selected_originals(
            vec![package_claim()],
            &evidence,
            SourceSelectionContext {
                interfaces: &packaged_interfaces,
                ..context()
            }
        )
        .is_ok());
        std::fs::write(&package, b"changed package interface").unwrap();
        assert!(validate_source_selected_originals(
            vec![package_claim()],
            &evidence,
            SourceSelectionContext {
                interfaces: &packaged_interfaces,
                ..context()
            }
        )
        .is_err());
        // A failed selection cannot contaminate a later valid request.
        assert!(validate_source_selected_originals(vec![claim()], &evidence, context()).is_ok());
    }

    #[test]
    fn canonical_source_selection_traverses_authored_edges_without_promoting_type_requirements() {
        let root = tempfile::tempdir().unwrap();
        let key = |name: &str| ExactModuleIdentity {
            unit: "main".into(),
            module: name.into(),
        };
        let a = root.path().join("A.hs");
        let b = root.path().join("B.hs");
        let a_bytes = b"module A where\nimport B\ntype Answer = B.Answer\n";
        let b_bytes = b"module B where\ntype Answer = Int\n";
        std::fs::write(&a, a_bytes).unwrap();
        std::fs::write(&b, b_bytes).unwrap();
        let b_interface = crate::certified_products::fixture_source_module_interface(
            [2; 32],
            "main",
            "B",
            Sha256::digest(b_bytes).into(),
            BTreeMap::new(),
            None,
        );
        let a_interface = crate::certified_products::fixture_source_module_interface(
            [2; 32],
            "main",
            "A",
            Sha256::digest(a_bytes).into(),
            BTreeMap::from([(("main".into(), "B".into()), b_interface.interface_sha256())]),
            None,
        );
        let a_interface = crate::certified_products::fixture_module_source_imports(
            a_interface,
            vec![crate::certified_products::CanonicalSourceImport {
                qualifier: ImportQualifier::Unqualified,
                module: "B".into(),
                boot: false,
                home_unit: Some("main".into()),
            }],
        );
        let interfaces = [a_interface.clone(), b_interface.clone()];
        let exact = interfaces
            .iter()
            .map(|interface| (key(interface.module()), interface.interface_sha256()))
            .collect::<BTreeMap<_, _>>();
        let evidence = DependencyEvidence {
            version: 4,
            cache_safe: true,
            selection_complete: true,
            sources: vec![
                SourceEvidence {
                    path: a.clone(),
                    sha256: hex(&a_interface.source_sha256()),
                },
                SourceEvidence {
                    path: b.clone(),
                    sha256: hex(&b_interface.source_sha256()),
                },
            ],
            modules: vec![
                ModuleEvidence {
                    unit: "main".into(),
                    module: "A".into(),
                    boot: false,
                    source: a.clone(),
                    product: ProductAvailability::InterfaceOnly,
                    imports: vec![ModuleImportEvidence {
                        qualifier: ImportQualifier::Unqualified,
                        module: "B".into(),
                        boot: false,
                        selected: Some(b.clone()),
                    }],
                },
                ModuleEvidence {
                    unit: "main".into(),
                    module: "B".into(),
                    boot: false,
                    source: b.clone(),
                    product: ProductAvailability::InterfaceOnly,
                    imports: vec![],
                },
            ],
            packages: vec![],
            resolutions: vec![
                ResolutionEvidence {
                    qualifier: ImportQualifier::Unqualified,
                    module: "A".into(),
                    boot: false,
                    selected: Some(a.clone()),
                    candidates: vec![a.clone()],
                },
                ResolutionEvidence {
                    qualifier: ImportQualifier::Unqualified,
                    module: "B".into(),
                    boot: false,
                    selected: Some(b.clone()),
                    candidates: vec![b.clone()],
                },
            ],
        };
        let fresh = DependencyEvidence {
            modules: vec![ModuleEvidence {
                unit: "main".into(),
                module: "Consumer".into(),
                boot: false,
                source: root.path().join("Consumer.hs"),
                product: ProductAvailability::Ready,
                imports: vec![],
            }],
            ..evidence.clone()
        };
        let roots = [(key("Consumer"), ImportQualifier::Unqualified, key("A"))];
        let includes = [root.path().to_path_buf()];
        let independent = BTreeSet::new();
        let source_less = BTreeSet::new();
        // Enter the private production issuer with an independently authenticated
        // root; the selection validator borrows the exact same checked owner.
        let role_path = root.path().join("Consumer.hs");
        let role_source = "module Consumer where\n";
        std::fs::write(&role_path, role_source).unwrap();
        let role_evidence = crate::cache::CompletedSourceEvidence::from_worker_evidence(
            DependencyEvidence {
                sources: vec![SourceEvidence {
                    path: role_path.clone(),
                    sha256: hex(&Sha256::digest(role_source.as_bytes()).into()),
                }],
                modules: vec![ModuleEvidence {
                    source: role_path.clone(),
                    ..fresh.modules[0].clone()
                }],
                version: 4,
                cache_safe: true,
                selection_complete: true,
                resolutions: vec![],
                packages: vec![],
            },
            &role_path,
            role_source,
        )
        .unwrap();
        let role_edges = vec![ExactReceiptModule {
            owner: roots[0].0.clone(),
            boot: false,
            imports: vec![ExactReceiptImport {
                qualifier: ImportQualifier::Unqualified,
                module: "A".into(),
                boot: false,
                unit: "main".into(),
            }],
        }];
        let role_row = RawSourceImportRole {
            qualifier: "none".into(),
            module: "A".into(),
            boot: false,
            unit: "main".into(),
            mask: 1,
        };
        let operation =
            tidepool_repr::execution_schema::InventoryOperation::new(Default::default());
        let import_roles = RootSourceImportRoles::issue(
            &role_evidence,
            &role_path,
            &role_edges,
            vec![role_row.clone()],
            |_| false,
            &operation,
        )
        .unwrap();
        let context = || SourceSelectionContext {
            producer: [2; 32],
            interfaces: &interfaces,
            exact_interfaces: &exact,
            include: &includes,
            fresh: &fresh,
            roots: &roots,
            import_roles: &import_roles,
            independent: &independent,
            source_less: &source_less,
        };
        let claims = || {
            interfaces
                .iter()
                .map(|interface| SourceSelectedOriginalClaim {
                    owner: key(interface.module()),
                    certificate_sha256: Sha256::digest(interface.certificate_bytes()).into(),
                    interface_sha256: interface.interface_sha256(),
                    source_sha256: interface.source_sha256(),
                })
                .collect::<Vec<_>>()
        };
        assert!(validate_source_selected_originals(claims(), &evidence, context()).is_ok());
        let missing = BTreeMap::from([(key("A"), a_interface.interface_sha256())]);
        assert!(validate_source_selected_originals(
            claims(),
            &evidence,
            SourceSelectionContext {
                exact_interfaces: &missing,
                ..context()
            }
        )
        .is_err());
        let changed = BTreeMap::from([
            (key("A"), a_interface.interface_sha256()),
            (key("B"), [9; 32]),
        ]);
        assert!(validate_source_selected_originals(
            claims(),
            &evidence,
            SourceSelectionContext {
                exact_interfaces: &changed,
                ..context()
            }
        )
        .is_err());
        let mut inventory_only = evidence.clone();
        inventory_only.modules[0].imports.clear();
        inventory_only.resolutions.pop();
        assert!(validate_source_selected_originals(claims(), &inventory_only, context()).is_err());
        let mut missing_child = evidence.clone();
        missing_child.modules.pop();
        missing_child.sources.pop();
        let mut only_a = claims();
        only_a.pop();
        assert!(validate_source_selected_originals(only_a, &missing_child, context()).is_err());
        // Drop both the reported edge and its claimed child. Canonical type
        // requirements still have B available, but cannot authenticate this lie.
        let mut omitted = evidence.clone();
        omitted.modules[0].imports.clear();
        omitted.modules.pop();
        omitted.sources.pop();
        omitted.resolutions.pop();
        let mut omitted_claims = claims();
        omitted_claims.pop();
        assert!(validate_source_selected_originals(omitted_claims, &omitted, context()).is_err());
        // A type requirement alone cannot manufacture an authored import.
        let type_only_a =
            crate::certified_products::fixture_module_source_imports(a_interface.clone(), vec![]);
        let type_only_interfaces = [type_only_a.clone(), b_interface.clone()];
        let mut promoted_claims = claims();
        promoted_claims[0].certificate_sha256 =
            Sha256::digest(type_only_a.certificate_bytes()).into();
        assert!(validate_source_selected_originals(
            promoted_claims,
            &evidence,
            SourceSelectionContext {
                interfaces: &type_only_interfaces,
                ..context()
            }
        )
        .is_err());
        let mut packages = evidence.clone();
        packages.modules[1].imports.push(ModuleImportEvidence {
            qualifier: ImportQualifier::Unqualified,
            module: "Data.List".into(),
            boot: false,
            selected: None,
        });
        assert!(validate_source_selected_originals(claims(), &packages, context()).is_err());
        assert!(validate_source_selected_originals(claims(), &evidence, context()).is_ok());
    }

    #[test]
    fn canonical_source_selection_allows_authenticated_edges_to_fresh_or_independent_owners() {
        let root = tempfile::tempdir().unwrap();
        let key = |name: &str| ExactModuleIdentity {
            unit: "main".into(),
            module: name.into(),
        };
        let a = root.path().join("A.hs");
        let b = root.path().join("B.hs");
        let bytes = b"module A where\nimport B ()\ntype Answer = Int\n";
        std::fs::write(&a, bytes).unwrap();
        std::fs::write(&b, b"module B where\nvalue = 43\n").unwrap();
        // This is a Rust boundary fixture, not a claim that GHC omits usage
        // for this syntax. The real compiler fixture inspects its own usages.
        let interface = crate::certified_products::fixture_module_source_imports(
            crate::certified_products::fixture_source_module_interface(
                [2; 32],
                "main",
                "A",
                Sha256::digest(bytes).into(),
                BTreeMap::new(),
                None,
            ),
            vec![crate::certified_products::CanonicalSourceImport {
                qualifier: ImportQualifier::Unqualified,
                module: "B".into(),
                boot: false,
                home_unit: Some("main".into()),
            }],
        );
        let interfaces = [interface.clone()];
        let exact = BTreeMap::from([(key("A"), interface.interface_sha256())]);
        let node = |name: &str, source: PathBuf| ModuleEvidence {
            unit: "main".into(),
            module: name.into(),
            boot: false,
            source,
            product: ProductAvailability::Ready,
            imports: vec![],
        };
        let evidence = DependencyEvidence {
            version: 4,
            cache_safe: true,
            selection_complete: true,
            sources: vec![SourceEvidence {
                path: a.clone(),
                sha256: hex(&interface.source_sha256()),
            }],
            modules: vec![ModuleEvidence {
                product: ProductAvailability::InterfaceOnly,
                imports: vec![ModuleImportEvidence {
                    qualifier: ImportQualifier::Unqualified,
                    module: "B".into(),
                    boot: false,
                    selected: Some(b.clone()),
                }],
                ..node("A", a.clone())
            }],
            packages: vec![],
            resolutions: [(&a, "A"), (&b, "B")]
                .into_iter()
                .map(|(path, name)| ResolutionEvidence {
                    qualifier: ImportQualifier::Unqualified,
                    module: name.into(),
                    boot: false,
                    selected: Some(path.clone()),
                    candidates: vec![path.clone()],
                })
                .collect(),
        };
        let fresh = DependencyEvidence {
            modules: vec![
                node("Consumer", root.path().join("Consumer.hs")),
                node("B", b),
            ],
            ..evidence.clone()
        };
        let roots = [(key("Consumer"), ImportQualifier::Unqualified, key("A"))];
        let includes = [root.path().to_path_buf()];
        let independent = BTreeSet::new();
        let source_less = BTreeSet::new();
        // Enter the private production issuer with an independently authenticated
        // root; the selection validator borrows the exact same checked owner.
        let role_path = root.path().join("Consumer.hs");
        let role_source = "module Consumer where\n";
        std::fs::write(&role_path, role_source).unwrap();
        let role_evidence = crate::cache::CompletedSourceEvidence::from_worker_evidence(
            DependencyEvidence {
                sources: vec![SourceEvidence {
                    path: role_path.clone(),
                    sha256: hex(&Sha256::digest(role_source.as_bytes()).into()),
                }],
                modules: vec![ModuleEvidence {
                    source: role_path.clone(),
                    ..fresh.modules[0].clone()
                }],
                version: 4,
                cache_safe: true,
                selection_complete: true,
                resolutions: vec![],
                packages: vec![],
            },
            &role_path,
            role_source,
        )
        .unwrap();
        let role_edges = vec![ExactReceiptModule {
            owner: roots[0].0.clone(),
            boot: false,
            imports: vec![ExactReceiptImport {
                qualifier: ImportQualifier::Unqualified,
                module: "A".into(),
                boot: false,
                unit: "main".into(),
            }],
        }];
        let role_row = RawSourceImportRole {
            qualifier: "none".into(),
            module: "A".into(),
            boot: false,
            unit: "main".into(),
            mask: 1,
        };
        let operation =
            tidepool_repr::execution_schema::InventoryOperation::new(Default::default());
        let import_roles = RootSourceImportRoles::issue(
            &role_evidence,
            &role_path,
            &role_edges,
            vec![role_row.clone()],
            |_| false,
            &operation,
        )
        .unwrap();
        let context = || SourceSelectionContext {
            producer: [2; 32],
            interfaces: &interfaces,
            exact_interfaces: &exact,
            include: &includes,
            fresh: &fresh,
            roots: &roots,
            import_roles: &import_roles,
            independent: &independent,
            source_less: &source_less,
        };
        let claims = || {
            vec![SourceSelectedOriginalClaim {
                owner: key("A"),
                certificate_sha256: Sha256::digest(interface.certificate_bytes()).into(),
                interface_sha256: interface.interface_sha256(),
                source_sha256: interface.source_sha256(),
            }]
        };
        assert!(validate_source_selected_originals(claims(), &evidence, context()).is_ok());
        let mut without_b = fresh.clone();
        without_b.modules.pop();
        assert!(validate_source_selected_originals(
            claims(),
            &evidence,
            SourceSelectionContext {
                fresh: &without_b,
                ..context()
            }
        )
        .is_err());
        let admitted = BTreeSet::from([key("B")]);
        assert!(validate_source_selected_originals(
            claims(),
            &evidence,
            SourceSelectionContext {
                fresh: &without_b,
                independent: &admitted,
                ..context()
            }
        )
        .is_ok());
        let mut pathless = evidence.clone();
        pathless.modules[0].imports[0].selected = None;
        pathless.resolutions.pop();
        let mut exact_home = exact.clone();
        exact_home.insert(key("B"), [7; 32]);
        let source_less = BTreeSet::from([key("B")]);
        let admitted_context = || SourceSelectionContext {
            exact_interfaces: &exact_home,
            fresh: &without_b,
            independent: &admitted,
            source_less: &source_less,
            ..context()
        };
        assert!(
            validate_source_selected_originals(claims(), &pathless, admitted_context()).is_ok()
        );
        for refused_context in [
            SourceSelectionContext {
                exact_interfaces: &exact,
                ..admitted_context()
            },
            SourceSelectionContext {
                independent: &independent,
                ..admitted_context()
            },
            SourceSelectionContext {
                source_less: &independent,
                ..admitted_context()
            },
        ] {
            assert!(
                validate_source_selected_originals(claims(), &pathless, refused_context).is_err()
            );
        }
        assert!(
            validate_source_selected_originals(claims(), &evidence, admitted_context()).is_err()
        );
        let mut packaged = pathless.clone();
        packaged.packages.push("B".into());
        assert!(
            validate_source_selected_originals(claims(), &packaged, admitted_context()).is_err()
        );
        let mut qualified = pathless;
        qualified.modules[0].imports[0].qualifier = ImportQualifier::OtherUnit("other".into());
        assert!(
            validate_source_selected_originals(claims(), &qualified, admitted_context()).is_err()
        );
        let mut omitted = evidence.clone();
        omitted.modules[0].imports.clear();
        omitted.resolutions.pop();
        assert!(validate_source_selected_originals(claims(), &omitted, context()).is_err());
    }
}
