//! Source visibility is issued from authenticated receipts, separately from
//! the compiler dependency graph and protected template imports.

use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceAdmissionBoundary {
    FiniteRequest,
    WorldStage(usize),
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
    pub(super) root: ExactModuleIdentity,
    rows: Vec<RawSourceImportRole>,
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
        let mut insert = |qualifier: String, module: String, boot: bool, unit: String| {
            operation.reserve::<(String, String, bool, String)>(1)
                .and_then(|_| operation.charge(qualifier.len().saturating_add(module.len()).saturating_add(unit.len())))
                .map_err(|error| compiler_evidence_failure(error.into()))?;
            expected.insert((qualifier, module, boot, unit));
            Ok::<_, CompileError>(())
        };
        let node = evidence.modules.iter().find(|node| {
            !node.boot && node.unit == root.unit && node.module == root.module
        }).ok_or_else(|| failure("source roles lack their authenticated root"))?;
        for edge in &node.imports {
            if edge.selected.is_some() {
                insert(String::from(edge.qualifier.clone()), edge.module.clone(), edge.boot, root.unit.clone())?;
            }
        }
        for module in exact.iter().filter(|module| !module.boot && module.owner == root) {
            for edge in &module.imports {
                insert(String::from(edge.qualifier.clone()), edge.module.clone(), edge.boot, edge.unit.clone())?;
            }
        }
        if !expected.iter().map(|(q,m,b,u)| (q.as_str(),m.as_str(),*b,u.as_str()))
            .eq(rows.iter().map(|row| (row.qualifier.as_str(),row.module.as_str(),row.boot,row.unit.as_str()))) {
            return Err(failure("source import roles differ from authenticated root HOME edges"));
        }
        let owner = Self { root, rows };
        for row in &owner.rows {
            let imported = identity(&row.unit, &row.module);
            if owner.role(&owner.root, &imported) == RootImportRole::InheritedLexical
                && !prior_lexical(&imported) {
                return Err(failure("inherited source import lacks an already-admitted lexical owner"));
            }
        }
        Ok(Arc::new(owner))
    }

    fn role(&self, source: &ExactModuleIdentity, imported: &ExactModuleIdentity) -> RootImportRole {
        if source != &self.root {
            return RootImportRole::AuthoredCurrent;
        }
        let mask = self.rows.iter().filter(|row| row.unit == imported.unit && row.module == imported.module)
            .fold(0, |mask, row| mask | row.mask);
        if mask & 1 != 0 {
            RootImportRole::AuthoredCurrent
        } else if mask & 2 != 0 {
            RootImportRole::InheritedLexical
        } else {
            RootImportRole::ProtectedCompiler
        }
    }

    pub(crate) fn requires_current_source(&self, source: &ExactModuleIdentity, imported: &ExactModuleIdentity) -> bool {
        self.role(source, imported) == RootImportRole::AuthoredCurrent
    }

    fn admits_lexical_edge(&self, source: &ExactModuleIdentity, imported: &ExactModuleIdentity) -> bool {
        self.role(source, imported) != RootImportRole::ProtectedCompiler
    }
}

/// Pre-product claims have already passed source/path/import authentication.
/// Only the product-bound issuer below can turn them into source admissions.
#[derive(Clone)]
pub(super) struct ReceiptSourceClaims {
    operation: Arc<tidepool_repr::execution_schema::InventoryOperation>,
    compilation_root: ExactModuleIdentity,
    dependencies: CompilerDependencies,
    lexical: BTreeMap<ExactModuleIdentity, SourceLexicalClaim>,
    pub(super) roles: Arc<RootSourceImportRoles>,
}

impl ReceiptSourceClaims {
    pub(super) fn issue(
        evidence: &crate::cache::CompletedSourceEvidence,
        compilation_root: ExactModuleIdentity,
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
            let inherited = roles.role(&compilation_root, &owner) == RootImportRole::InheritedLexical;
            let kind = if inherited {
                if !prior(&owner) {
                    return Err(failure("inherited source traversal lacks prior source admission"));
                }
                SourceLexicalClaim::PriorProgramSource
            } else if selected.contains_key(&owner) {
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
                    .filter(|child| roles.admits_lexical_edge(&owner, child))
                    .cloned(),
            );
        }
        Ok(Self {
            operation: Arc::clone(operation),
            compilation_root,
            dependencies: CompilerDependencies { adjacency },
            lexical,
            roles,
        })
    }

    pub(super) fn compilation_root(&self) -> &ExactModuleIdentity {
        &self.compilation_root
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
    operation
        .reserve::<(ExactModuleIdentity, Vec<ExactModuleIdentity>)>(copies)
        .and_then(|_| {
            operation.charge(
                owner
                    .unit
                    .len()
                    .saturating_add(owner.module.len())
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
            if bundle.roles.insert(roles.root.clone(), Arc::clone(roles))
                .is_some_and(|previous| previous != *roles) {
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
                    .filter(|child| self.roles.get(owner).is_none_or(|roles| roles.admits_lexical_edge(owner, child)))
                    .cloned()
                    .collect()
            })
    }
}
