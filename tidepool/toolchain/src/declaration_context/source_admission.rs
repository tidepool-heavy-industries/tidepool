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
#[derive(Clone, Debug, Default)]
pub(crate) struct CompilerDependencies {
    adjacency: BTreeMap<ExactModuleIdentity, Vec<ExactModuleIdentity>>,
}

impl CompilerDependencies {
    pub(super) fn merge<'a>(
        admissions: impl IntoIterator<Item = &'a ExactSourceAdmission>,
    ) -> Result<Self, CompileError> {
        let mut result = Self::default();
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

/// Exact template-authorized edges, not a capability to import their owners
/// from authored code. The recipe itself remains with the receipt input owner.
#[derive(Clone, Debug, Default)]
struct ProtectedTemplateLinks {
    edges: BTreeMap<ExactModuleIdentity, BTreeSet<ExactModuleIdentity>>,
    authored_overlaps: BTreeMap<ExactModuleIdentity, BTreeSet<ExactModuleIdentity>>,
}

impl ProtectedTemplateLinks {
    fn excludes_lexical(
        &self,
        source: &ExactModuleIdentity,
        imported: &ExactModuleIdentity,
    ) -> bool {
        self.edges
            .get(source)
            .is_some_and(|edges| edges.contains(imported))
            && !self
                .authored_overlaps
                .get(source)
                .is_some_and(|edges| edges.contains(imported))
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
    protected: ProtectedTemplateLinks,
}

impl ReceiptSourceClaims {
    pub(super) fn issue(
        evidence: &crate::cache::CompletedSourceEvidence,
        compilation_root: ExactModuleIdentity,
        exact_imports: &BTreeMap<ExactModuleIdentity, Vec<ExactModuleIdentity>>,
        selected: &BTreeMap<ExactModuleIdentity, crate::execution_source::SourceSelectedOriginal>,
        protected_edges: BTreeMap<ExactModuleIdentity, BTreeSet<ExactModuleIdentity>>,
        authored_overlaps: BTreeMap<ExactModuleIdentity, BTreeSet<ExactModuleIdentity>>,
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
        let protected = ProtectedTemplateLinks {
            edges: protected_edges,
            authored_overlaps,
        };
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
            // Only an edge admitted by the exact protected recipe is excluded.
            // The same owner reached by an authored edge still needs a claim.
            pending.extend(
                imports
                    .iter()
                    .filter(|child| !protected.excludes_lexical(&owner, child))
                    .cloned(),
            );
        }
        Ok(Self {
            operation: Arc::clone(operation),
            compilation_root,
            dependencies: CompilerDependencies { adjacency },
            lexical,
            protected,
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
    protected: ProtectedTemplateLinks,
}

impl std::fmt::Debug for SourceAdmissionBundle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SourceAdmissionBundle")
            .field("dependencies", &self.dependencies.adjacency.len())
            .field("claims", &self.lexical.claims.len())
            .field("protected_sources", &self.protected.edges.len())
            .finish()
    }
}

impl SourceAdmissionBundle {
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
            protected: Default::default(),
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
            for (owner, edges) in &admission.claims.protected.edges {
                charge_owner(&admission.claims.operation, owner, 1)?;
                for child in edges {
                    charge_owner(&admission.claims.operation, child, 1)?;
                }
                bundle
                    .protected
                    .edges
                    .entry(owner.clone())
                    .or_default()
                    .extend(edges.iter().cloned());
            }
            for (source, imports) in &admission.claims.protected.authored_overlaps {
                charge_owner(&admission.claims.operation, source, 1)?;
                for imported in imports {
                    charge_owner(&admission.claims.operation, imported, 1)?;
                }
                bundle
                    .protected
                    .authored_overlaps
                    .entry(source.clone())
                    .or_default()
                    .extend(imports.iter().cloned());
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
            protected_sources = bundle.protected.edges.len(),
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
                    .filter(|child| !self.protected.excludes_lexical(owner, child))
                    .cloned()
                    .collect()
            })
    }
}
