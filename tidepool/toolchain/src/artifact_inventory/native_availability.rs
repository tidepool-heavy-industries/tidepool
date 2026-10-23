use super::compiler_projection::SelectedNativeSupport;
use super::*;

/// Exact dependency grants borrow the actual selected source owner.
pub(crate) struct NativeDependencyCatalog<'a>(&'a CompilerSourceSupport);

impl<'a> NativeDependencyCatalog<'a> {
    pub(crate) fn from_source(source: &'a CompilerSourceSupport) -> Result<Self, CompileError> {
        Ok(Self(source))
    }

    fn selected_support(
        &self,
        owner: &ExactModuleIdentity,
        group: NativeGroupBinding,
    ) -> Option<&SelectedNativeSupport> {
        self.0
            .compiler_roles()
            .selected_native_support(owner, group)
    }

    pub(super) fn original_namespace_pair(
        &self,
        owner: &ExactModuleIdentity,
    ) -> Result<Option<(ArtifactBinding, ArtifactBinding)>, CompileError> {
        Ok(self.0.world_primary_type_row(owner).and_then(|row| {
            row.original_binding
                .map(|original| (*original, *row.interface_binding))
        }))
    }

    pub(crate) fn decision<'b>(&'b self, owner: &ExactModuleIdentity) -> InputNativeDecision<'b> {
        self.0.compiler_roles().native_decision(owner)
    }

    fn group_supports(
        &self,
        key: NativeGroupKey,
        owner: &tidepool_repr::execution_schema::CachedHomeOwner,
    ) -> Vec<(NativeGroupBinding, &IssuedNativeOffer)> {
        let mut supports = BTreeMap::new();
        if let InputNativeDecision::IssuedChoices(choices) = self.decision(&module_identity(owner))
        {
            if let Some(group) = choices
                .get(&key.original_ordinal)
                .filter(|group| group.binding.artifact() == key.artifact)
            {
                if let Some(selected) = self.selected_support(&module_identity(owner), *group) {
                    if let Some(offer) = selected
                        .acquisitions()
                        .iter()
                        .find(|offer| offer.contains_checked_group(*group))
                    {
                        supports.insert(*group, offer);
                    }
                }
                return supports.into_iter().collect();
            }
        }
        // A fresh product's emitted dependency may consume available input
        // code without choosing that code as an older executable root. An
        // explicitly selected group above must keep its exact acquisition.
        if let Some(support) = self.0.native_availability().dependency_support(key, owner) {
            for group in support.groups() {
                if let Some(offer) = support.offer_for_group(group) {
                    supports.entry(group).or_insert(offer);
                }
            }
        }
        supports.into_iter().collect()
    }
}

/// One operation's chosen exact bindings and their inseparable acquisition.
/// Payload and file custody are staged until the receiving admission succeeds.
pub(super) struct CertifiedNativeDemandAcquisition {
    pub(super) entries: BTreeMap<ArtifactId, Arc<ArtifactEntry>>,
    pub(super) bindings: BTreeMap<NativeGroupKey, VerifiedNativeBinding>,
    pub(super) offers: Vec<IssuedNativeOffer>,
}

/// An issued binding retains its exact carrier pair and acquisition closure.
/// Only physical current output can request a not-yet-issued group.
pub(super) enum VerifiedNativeBinding {
    Issued(IssuedNativeBinding),
    CurrentIssued(IssuedNativeBinding),
    NewCurrent {
        dependencies: Vec<ResolvedNativeDependency>,
    },
}

/// Constructed only with the matching acquisition owner below.
#[derive(Clone, Debug)]
pub(super) struct IssuedNativeBinding {
    binding: NativeGroupBinding,
    carrier: ArtifactBinding,
    canonical: ArtifactBinding,
    closure: BTreeSet<InventoryNodeKey>,
    source: NativeSourceCause,
}

/// A reached target occurrence can only select an already issued group.
pub(super) struct TargetGroupSelection {
    pub(super) issued: IssuedNativeBinding,
    pub(super) offer: IssuedNativeOffer,
    pub(super) entry: Arc<ArtifactEntry>,
    pub(super) sources: NativeSourceReceipts,
}

pub(super) fn select_target_group(
    state: &mut InventoryState,
    physical: &ArtifactView,
    selection: &crate::certified_products::CertifiedSourceSelection,
    reference: &crate::certified_products::ReceiptImportOwner,
    operation: &tidepool_repr::execution_schema::InventoryOperation,
    incoming: &mut BTreeMap<ArtifactId, Arc<ArtifactEntry>>,
) -> Result<TargetGroupSelection, CompileError> {
    use crate::certified_products::{CertifiedNativeGroupSupport, ReceiptImportOwner};
    let evidence = |error| CompileError::CompilerEvidence(Box::new(error));
    let (binding, offer, current) = match reference {
        ReceiptImportOwner::OriginalBinding { .. } => {
            let chosen = selection.target_original_group(reference, operation)?;
            (
                chosen.binding(),
                chosen.contribution().offer().clone(),
                false,
            )
        }
        ReceiptImportOwner::Source { .. } => {
            let (entry, ordinal) = selection.target_source_entry(reference).map_err(evidence)?;
            let ArtifactPayload::Original(product) = &entry.payload else {
                return Err(failure("target Source lacks Original payload"));
            };
            let key = NativeGroupKey {
                artifact: entry.descriptor.id,
                original_ordinal: ordinal,
            };
            match selection
                .target_source_origin(key, &entry)
                .map_err(evidence)?
            {
                crate::certified_products::TargetSourceOrigin::Current(current) => {
                    let completed = current.completed_for_target(physical)?;
                    let (binding, offer) = completed_target_group(&completed)?;
                    (binding, offer, true)
                }
                crate::certified_products::TargetSourceOrigin::PriorProgram(completed) => {
                    let (binding, offer) = completed_target_group(&completed)?;
                    (binding, offer, false)
                }
                crate::certified_products::TargetSourceOrigin::SelectedInput => {
                    if !selection.permits_native_group(key.artifact, product.owner(), ordinal) {
                        return Err(native_admission_failure(
                            key,
                            NativeAdmissionRefusal::InputPermissionDenied {
                                body_present: entry.native_group_ordinals.contains(&ordinal),
                                selected_original: physical
                                    .lease
                                    .selection
                                    .namespace
                                    .get(&key.artifact)
                                    .copied(),
                            },
                        ));
                    }
                    let binding = verified_root_choice(selection, key, product.owner(), &entry)?;
                    let support = selection
                        .native_group_support(key, product.owner(), binding)
                        .ok_or_else(|| {
                            failure("target input group lost its chosen contribution")
                        })?;
                    let CertifiedNativeGroupSupport::Issued(offer) = support else {
                        return Err(failure("target input group lacks issued availability"));
                    };
                    (binding, offer.clone(), false)
                }
            }
        }
        _ => return Err(failure("target native reference kind")),
    };
    retain_target_group(state, physical, binding, offer, current, incoming)
}

fn completed_target_group(
    completed: &crate::certified_products::CompletedTargetProductRef<'_>,
) -> Result<(NativeGroupBinding, IssuedNativeOffer), CompileError> {
    let mut bindings = completed.groups();
    let binding = bindings
        .next()
        .ok_or_else(|| failure("target produced group was not issued"))?;
    if bindings.next().is_some() {
        return Err(failure("target produced group has competing own issuers"));
    }
    let offer = completed
        .support(binding)
        .ok_or_else(|| failure("target produced group lost its own contribution"))?
        .clone();
    Ok((binding, offer))
}

pub(super) fn select_target_entry(
    state: &mut InventoryState,
    physical: &ArtifactView,
    selection: &crate::certified_products::CertifiedSourceSelection,
    entry: &crate::checked_cell::CheckedTypedEntry,
    incoming: &mut BTreeMap<ArtifactId, Arc<ArtifactEntry>>,
) -> Result<TargetGroupSelection, CompileError> {
    let key = entry.native_group_key();
    let payload = physical
        .observed_payload(&key.artifact)
        .ok_or_else(|| failure("typed target entry lacks its physical product"))?;
    let current = selection
        .current_product(key, payload)
        .map_err(|error| CompileError::CompilerEvidence(Box::new(error)))?
        .ok_or_else(|| failure("typed target entry is outside its physical output cohort"))?;
    let completed = current.completed_for_target(physical)?;
    let (binding, offer) = completed_target_group(&completed)?;
    retain_target_group(state, physical, binding, offer, true, incoming)
}

fn retain_target_group(
    state: &mut InventoryState,
    physical: &ArtifactView,
    binding: NativeGroupBinding,
    offer: IssuedNativeOffer,
    current: bool,
    incoming: &mut BTreeMap<ArtifactId, Arc<ArtifactEntry>>,
) -> Result<TargetGroupSelection, CompileError> {
    let support = NativeSupportRef::checked(
        state,
        physical,
        binding,
        crate::certified_products::CertifiedNativeGroupSupport::Issued(&offer),
    )?;
    let mut issued = support.describe(binding)?;
    support.acquire(state, physical, incoming, &issued)?;
    let entry = Arc::clone(
        support
            .payloads
            .get(&binding.binding.artifact())
            .ok_or_else(|| failure("target group lost its exact payload"))?,
    );
    let mut sources = NativeSourceReceipts::default();
    if current {
        let NativeOfferSourceReceipt::Solved(frozen) = &offer.0.custody.source_receipt else {
            return Err(failure(
                "target current group lost its solved source causes",
            ));
        };
        for (usage, causes) in frozen.entries.iter() {
            let reached = match usage {
                NativeSourceUse::Root(group) => {
                    issued.closure.contains(&InventoryNodeKey::Group(*group))
                }
                NativeSourceUse::Edge { parent, child } => {
                    issued.closure.contains(&InventoryNodeKey::Group(*parent))
                        && issued.closure.contains(&InventoryNodeKey::Group(*child))
                }
            };
            if reached {
                for cause in causes.iter() {
                    sources.insert(*usage, cause.clone());
                }
            }
        }
        issued.source = NativeSourceCause::Current;
    }
    sources.insert(NativeSourceUse::Root(binding), issued.source.clone());
    Ok(TargetGroupSelection {
        issued,
        offer,
        entry,
        sources,
    })
}

impl IssuedNativeBinding {
    pub(super) fn binding(&self) -> NativeGroupBinding {
        self.binding
    }
}

impl VerifiedNativeBinding {
    pub(super) fn source_cause(&self) -> NativeSourceCause {
        match self {
            Self::Issued(issued) => issued.source.clone(),
            Self::CurrentIssued(_) | Self::NewCurrent { .. } => NativeSourceCause::Current,
        }
    }
    pub(super) fn binding(&self) -> Option<NativeGroupBinding> {
        match self {
            Self::Issued(issued) | Self::CurrentIssued(issued) => Some(issued.binding),
            Self::NewCurrent { .. } => None,
        }
    }

    pub(super) fn carrier_pair(&self) -> Option<(ArtifactBinding, ArtifactBinding)> {
        match self {
            Self::Issued(issued) | Self::CurrentIssued(issued) => {
                Some((issued.carrier, issued.canonical))
            }
            Self::NewCurrent { .. } => None,
        }
    }

    #[cfg(test)]
    fn closure(&self) -> &BTreeSet<InventoryNodeKey> {
        match self {
            Self::Issued(issued) | Self::CurrentIssued(issued) => &issued.closure,
            Self::NewCurrent { .. } => panic!("new current group has no issued closure"),
        }
    }
}

/// An already issued dependency or a physical product in this output cohort.
#[derive(Clone, Debug)]
pub(super) enum NativeGroupTarget {
    ExactGroup(Arc<IssuedNativeBinding>),
    CurrentIssued(Arc<IssuedNativeBinding>),
    NewGroup(NativeGroupKey),
}

impl NativeGroupTarget {
    pub(super) fn source_cause(&self) -> NativeSourceCause {
        match self {
            Self::ExactGroup(issued) => issued.source.clone(),
            Self::CurrentIssued(_) | Self::NewGroup(_) => NativeSourceCause::Current,
        }
    }
}

/// A native obligation includes its required owner: ordinal labels alone
/// cannot distinguish two imported modules with the same original ordinal.
#[derive(Clone, Debug)]
pub(super) struct ResolvedNativeDependency {
    required_owner: ExactModuleIdentity,
    edge: NativeGroupEdge,
    target: NativeGroupTarget,
}

impl ResolvedNativeDependency {
    pub(super) fn target_key(&self) -> NativeGroupKey {
        match &self.target {
            NativeGroupTarget::ExactGroup(issued) | NativeGroupTarget::CurrentIssued(issued) => {
                NativeGroupKey {
                    artifact: issued.binding.binding.artifact(),
                    original_ordinal: issued.binding.original_ordinal,
                }
            }
            NativeGroupTarget::NewGroup(key) => *key,
        }
    }

    pub(super) fn planned_edge(
        &self,
        parent: NativeGroupKey,
        child: &ArtifactEntry,
    ) -> Result<(PlannedTarget, ArtifactDependency), CompileError> {
        let target = self.target_key();
        if self.edge.dependent_ordinal != parent.original_ordinal
            || self.edge.required_ordinal != target.original_ordinal
            || child.descriptor.id != target.artifact
            || child.descriptor.owner != self.required_owner
            || !child
                .native_group_ordinals
                .contains(&target.original_ordinal)
        {
            return Err(failure(
                "carried native dependency differs from its physical obligation",
            ));
        }
        Ok((
            PlannedTarget::from(self.target.clone()),
            ArtifactDependency::NativeGroup {
                dependent_ordinal: self.edge.dependent_ordinal,
                required_ordinal: self.edge.required_ordinal,
            },
        ))
    }

    pub(super) fn target(&self) -> &NativeGroupTarget {
        &self.target
    }
}

enum CohortNativeRequirement {
    Local {
        owner: ExactModuleIdentity,
        edge: NativeGroupEdge,
        child: NativeGroupKey,
    },
    External {
        owner: ExactModuleIdentity,
        edge: NativeGroupEdge,
        support: crate::declaration_context::ChosenNativeGroupSupport,
    },
    Unavailable,
}

/// All nominal group obligations of one physical output cohort are classified
/// once. Unavailable obligations remain explicit until local closure is removed.
pub(super) struct ClassifiedNativeCohort {
    operation: Arc<tidepool_repr::execution_schema::InventoryOperation>,
    entries: Vec<Arc<ArtifactEntry>>,
    requirements: BTreeMap<NativeGroupKey, Vec<CohortNativeRequirement>>,
    available: BTreeSet<NativeGroupKey>,
}

impl ClassifiedNativeCohort {
    pub(super) fn classify(
        physical: &ArtifactView,
        selection: &crate::certified_products::CertifiedSourceSelection,
        operation: &Arc<tidepool_repr::execution_schema::InventoryOperation>,
    ) -> Result<Self, CompileError> {
        use crate::certified_products::{CurrentSourceDependency, ReceiptImportOwner};
        let evidence = |error| CompileError::CompilerEvidence(Box::new(error));
        let entries = selection
            .complete_native_entries(physical, operation)
            .map_err(evidence)?;
        let metadata = physical.metadata_snapshot();
        let mut requirements = BTreeMap::new();
        let mut reverse = BTreeMap::<NativeGroupKey, BTreeSet<NativeGroupKey>>::new();
        let mut blocked = BTreeSet::new();
        for entry in &entries {
            for &ordinal in &entry.native_group_ordinals {
                crate::host_work::checkpoint()?;
                let key = NativeGroupKey {
                    artifact: entry.descriptor.id,
                    original_ordinal: ordinal,
                };
                let current = selection
                    .current_product(key, entry)
                    .map_err(evidence)?
                    .ok_or_else(|| {
                        failure("cohort requirement lacks its actual current product")
                    })?;
                let imports = current.nominal_imports().map_err(evidence)?;
                operation
                    .reserve::<CohortNativeRequirement>(imports.len())
                    .map_err(|error| evidence(error.into()))?;
                let mut obligations = Vec::new();
                for import in imports {
                    let reference = import.reference();
                    crate::host_work::checkpoint()?;
                    operation
                        .charge(1)
                        .map_err(|error| evidence(error.into()))?;
                    let (unit, module) = match reference {
                        ReceiptImportOwner::Source { unit, module, .. }
                        | ReceiptImportOwner::OriginalBinding { unit, module, .. } => {
                            (unit, module)
                        }
                        _ => continue,
                    };
                    operation
                        .charge(unit.len() + module.len())
                        .map_err(|error| evidence(error.into()))?;
                    let owner = ExactModuleIdentity {
                        unit: unit.clone(),
                        module: module.clone(),
                    };
                    let requirement = match reference {
                        ReceiptImportOwner::Source { .. } => match import
                            .available_source_dependency(&metadata.artifacts, operation)?
                        {
                            CurrentSourceDependency::Local(child) => {
                                let child = child.key();
                                operation
                                    .reserve::<(NativeGroupKey, [usize; 4])>(2)
                                    .map_err(|error| evidence(error.into()))?;
                                reverse.entry(child).or_default().insert(key);
                                CohortNativeRequirement::Local {
                                    owner,
                                    edge: NativeGroupEdge {
                                        dependent_ordinal: ordinal,
                                        required_ordinal: child.original_ordinal,
                                    },
                                    child,
                                }
                            }
                            CurrentSourceDependency::Issued(support) => {
                                CohortNativeRequirement::External {
                                    owner,
                                    edge: NativeGroupEdge {
                                        dependent_ordinal: ordinal,
                                        required_ordinal: support.binding().original_ordinal,
                                    },
                                    support,
                                }
                            }
                            CurrentSourceDependency::Unavailable => {
                                CohortNativeRequirement::Unavailable
                            }
                        },
                        ReceiptImportOwner::OriginalBinding { .. } => {
                            match current.available_original_group(reference, operation)? {
                                Some(support) => CohortNativeRequirement::External {
                                    owner,
                                    edge: NativeGroupEdge {
                                        dependent_ordinal: ordinal,
                                        required_ordinal: support.binding().original_ordinal,
                                    },
                                    support,
                                },
                                None => CohortNativeRequirement::Unavailable,
                            }
                        }
                        _ => unreachable!("native nominal requirement classified above"),
                    };
                    if matches!(requirement, CohortNativeRequirement::Unavailable) {
                        operation
                            .reserve::<(NativeGroupKey, [usize; 4])>(1)
                            .map_err(|error| evidence(error.into()))?;
                        blocked.insert(key);
                    }
                    obligations.push(requirement);
                }
                operation
                    .reserve::<(NativeGroupKey, Vec<CohortNativeRequirement>, [usize; 4])>(1)
                    .map_err(|error| evidence(error.into()))?;
                requirements.insert(key, obligations);
            }
        }
        operation
            .reserve::<NativeGroupKey>(blocked.len())
            .map_err(|error| evidence(error.into()))?;
        let mut pending = blocked.iter().copied().collect::<Vec<_>>();
        while let Some(child) = pending.pop() {
            crate::host_work::checkpoint()?;
            for &parent in reverse.get(&child).into_iter().flatten() {
                operation
                    .charge(1)
                    .map_err(|error| evidence(error.into()))?;
                if !blocked.contains(&parent) {
                    operation
                        .reserve::<NativeGroupKey>(2)
                        .map_err(|error| evidence(error.into()))?;
                    blocked.insert(parent);
                    pending.push(parent);
                }
            }
        }
        operation
            .reserve::<(NativeGroupKey, [usize; 4])>(requirements.len())
            .map_err(|error| evidence(error.into()))?;
        let available = requirements
            .keys()
            .filter(|key| !blocked.contains(key))
            .copied()
            .collect();
        Ok(Self {
            operation: Arc::clone(operation),
            entries,
            requirements,
            available,
        })
    }

    pub(super) fn available(&self) -> &BTreeSet<NativeGroupKey> {
        &self.available
    }
    pub(super) fn entries(&self) -> &[Arc<ArtifactEntry>] {
        &self.entries
    }
}

pub(super) fn bind_classified_cohort(
    state: &mut InventoryState,
    parent: &ArtifactView,
    entries: &impl ArtifactEntryMap<ArtifactId>,
    cohort: &ClassifiedNativeCohort,
) -> Result<CertifiedNativeDemandAcquisition, CompileError> {
    let mut acquisition = CertifiedNativeDemandAcquisition {
        entries: BTreeMap::new(),
        bindings: BTreeMap::new(),
        offers: Vec::new(),
    };
    for &key in &cohort.available {
        cohort
            .operation
            .reserve::<ResolvedNativeDependency>(cohort.requirements[&key].len())
            .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
        let mut dependencies = Vec::with_capacity(cohort.requirements[&key].len());
        for requirement in &cohort.requirements[&key] {
            crate::host_work::checkpoint()?;
            let (required_owner, edge, target) = match requirement {
                CohortNativeRequirement::Local { owner, edge, child } => {
                    if !cohort.available.contains(child) {
                        return Err(failure(
                            "available cohort group retains unavailable local dependency",
                        ));
                    }
                    (owner.clone(), *edge, NativeGroupTarget::NewGroup(*child))
                }
                CohortNativeRequirement::External {
                    owner,
                    edge,
                    support,
                } => {
                    let binding = support.binding();
                    let native = NativeSupportRef::checked(
                        state,
                        parent,
                        binding,
                        crate::certified_products::CertifiedNativeGroupSupport::Issued(
                            support.contribution().offer(),
                        ),
                    )?;
                    let issued = native.describe(binding)?;
                    native.acquire(state, entries, &mut acquisition.entries, &issued)?;
                    acquisition
                        .offers
                        .push(support.contribution().offer().clone());
                    (
                        owner.clone(),
                        *edge,
                        NativeGroupTarget::ExactGroup(Arc::new(issued)),
                    )
                }
                CohortNativeRequirement::Unavailable => {
                    return Err(failure(
                        "available cohort group retains an unavailable obligation",
                    ))
                }
            };
            dependencies.push(ResolvedNativeDependency {
                required_owner,
                edge,
                target,
            });
        }
        acquisition
            .bindings
            .insert(key, VerifiedNativeBinding::NewCurrent { dependencies });
    }
    Ok(acquisition)
}

/// One exact labeled native dependency edge from a parent group.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub(crate) struct NativeGroupEdge {
    pub(crate) dependent_ordinal: u32,
    pub(crate) required_ordinal: u32,
}

fn module_identity(
    owner: &tidepool_repr::execution_schema::CachedHomeOwner,
) -> ExactModuleIdentity {
    ExactModuleIdentity {
        unit: owner.unit.clone(),
        module: owner.module.clone(),
    }
}

fn root_refusal(key: NativeGroupKey, cause: NativeAdmissionRefusal) -> CompileError {
    native_admission_failure(key, cause)
}

/// Root provenance: consume the root's carried exact chosen binding. Missing,
/// explicit-empty, unavailable, and conflicting choices stay distinguishable.
/// An absent choice never becomes permission to select a retained candidate.
pub(super) fn verified_root_choice(
    selection: &crate::certified_products::CertifiedSourceSelection,
    key: NativeGroupKey,
    owner: &tidepool_repr::execution_schema::CachedHomeOwner,
    entry: &ArtifactEntry,
) -> Result<NativeGroupBinding, CompileError> {
    use crate::certified_products::{
        CertificationError, InputDecisionRefusal, NativeGroupCandidates,
    };
    let identity = module_identity(owner);
    let (support, native_owner) = match selection.continuation_input(&identity) {
        Ok(Some(input)) => input,
        Ok(None) => {
            return Err(root_refusal(
                key,
                NativeAdmissionRefusal::RootChoiceMissing { owner: identity },
            ))
        }
        Err(error @ CertificationError::InputDecision { .. }) => {
            let CertificationError::InputDecision { reason, .. } = &error else {
                unreachable!("arm narrowed to input decisions");
            };
            let cause = reason.clone();
            return Err(match root_evidence_refusal(&identity, cause) {
                Ok(refusal) => root_refusal(key, refusal),
                Err(_) => CompileError::CompilerEvidence(Box::new(error)),
            });
        }
        Err(error) => return Err(CompileError::CompilerEvidence(Box::new(error))),
    };
    let ArtifactPayload::Original(product) = &entry.payload else {
        return Err(failure("native root demand lacks original payload"));
    };
    let canonical = ArtifactEntry::canonical_artifact_id(product.module_interface());
    let choices = match support
        .checked_native_input_decision(key.artifact, canonical, native_owner)
        .map_err(|reason| match root_evidence_refusal(&identity, reason) {
            Ok(refusal) => root_refusal(key, refusal),
            Err(reason) => {
                CompileError::CompilerEvidence(Box::new(CertificationError::InputDecision {
                    owner: identity.clone(),
                    reason,
                }))
            }
        })? {
        InputNativeDecision::IssuedChoices(choices) => choices,
        InputNativeDecision::UnissuedPolicy => {
            return Err(root_refusal(
                key,
                NativeAdmissionRefusal::RootChoiceMissing { owner: identity },
            ))
        }
    };
    let Some(chosen) = choices.get(&key.original_ordinal) else {
        return Err(root_refusal(
            key,
            NativeAdmissionRefusal::RootChoiceExplicitEmpty {
                owner: identity,
                original_ordinal: key.original_ordinal,
            },
        ));
    };
    if chosen.binding.artifact() != key.artifact
        || chosen.original_ordinal != key.original_ordinal
        || !entry.native_group_ordinals.contains(&key.original_ordinal)
    {
        return Err(root_refusal(
            key,
            NativeAdmissionRefusal::RootChoiceConflicting {
                chosen: *chosen,
                recorded: None,
            },
        ));
    }
    // An exact carried choice is authority. Candidate availability only says
    // whether that chosen binding was actually issued here; it never selects a
    // different candidate when this one is absent.
    match selection.native_group_candidates(key, owner) {
        Ok(NativeGroupCandidates::IssuedAvailability(candidates))
            if candidates.contains(chosen) =>
        {
            Ok(*chosen)
        }
        Ok(NativeGroupCandidates::IssuedAvailability(_)) => Err(root_refusal(
            key,
            NativeAdmissionRefusal::RootChoiceUnavailable { chosen: *chosen },
        )),
        Ok(NativeGroupCandidates::NewProduct(_)) => Err(root_refusal(
            key,
            NativeAdmissionRefusal::RootChoiceConflicting {
                chosen: *chosen,
                recorded: None,
            },
        )),
        Err(CertificationError::InputDecision {
            reason: InputDecisionRefusal::UnavailableChosenBinding,
            ..
        }) => Err(root_refusal(
            key,
            NativeAdmissionRefusal::RootChoiceUnavailable { chosen: *chosen },
        )),
        Err(error @ CertificationError::InputDecision { .. }) => {
            let CertificationError::InputDecision { reason, .. } = &error else {
                unreachable!("arm narrowed to input decisions");
            };
            let cause = reason.clone();
            Err(match root_evidence_refusal(&identity, cause) {
                Ok(refusal) => root_refusal(key, refusal),
                Err(_) => CompileError::CompilerEvidence(Box::new(error)),
            })
        }
        Err(error) => Err(CompileError::CompilerEvidence(Box::new(error))),
    }
}

fn root_evidence_refusal(
    identity: &ExactModuleIdentity,
    reason: crate::certified_products::InputDecisionRefusal,
) -> Result<NativeAdmissionRefusal, crate::certified_products::InputDecisionRefusal> {
    use crate::certified_products::InputDecisionRefusal;
    Ok(match reason {
        InputDecisionRefusal::UnexpectedVersion => {
            NativeAdmissionRefusal::RootChoiceVersionMismatch {
                owner: identity.clone(),
            }
        }
        InputDecisionRefusal::MissingContinuationOwner | InputDecisionRefusal::MissingDecision => {
            NativeAdmissionRefusal::RootChoiceMissing {
                owner: identity.clone(),
            }
        }
        InputDecisionRefusal::NativeOwnerMismatch
        | InputDecisionRefusal::ArtifactIdentityMismatch(_)
        | InputDecisionRefusal::RoleMismatch => {
            NativeAdmissionRefusal::RootChoiceIdentityMismatch {
                owner: identity.clone(),
            }
        }
        InputDecisionRefusal::UnavailableChosenBinding => return Err(reason),
    })
}

/// Resolve the own binding of a positively completed product. A carried own
/// binding is reused; only an unissued current group may receive a new binding.
pub(super) fn verified_current_product(
    current: &crate::certified_products::CurrentProductRef<'_>,
) -> Result<Option<NativeGroupBinding>, CompileError> {
    use crate::certified_products::NativeGroupCandidates;
    let key = current.key();
    let candidates = current
        .own_group_candidates()
        .map_err(|error| CompileError::CompilerEvidence(Box::new(error)))?;
    let bindings = match candidates {
        NativeGroupCandidates::NewProduct(groups) if groups.is_empty() => return Ok(None),
        NativeGroupCandidates::NewProduct(groups)
        | NativeGroupCandidates::IssuedAvailability(groups) => groups,
    };
    let mut bindings = bindings.into_iter();
    let Some(chosen) = bindings.next() else {
        return Err(native_admission_failure(
            key,
            NativeAdmissionRefusal::CurrentProductNotCompleted {
                original: key.artifact,
                original_ordinal: key.original_ordinal,
            },
        ));
    };
    if bindings.next().is_some() {
        return Err(native_admission_failure(
            key,
            NativeAdmissionRefusal::CurrentProductConflicting {
                original: key.artifact,
                original_ordinal: key.original_ordinal,
            },
        ));
    }
    Ok(Some(chosen))
}

/// Resolve one physical product against its captured input owner and output
/// cohort. New cohort targets stay symbolic until cohort issuance completes.
fn current_product_dependencies(
    state: &mut InventoryState,
    parent: &ArtifactView,
    entries: &impl ArtifactEntryMap<ArtifactId>,
    incoming: &mut BTreeMap<ArtifactId, Arc<ArtifactEntry>>,
    offers: &mut Vec<IssuedNativeOffer>,
    current: &crate::certified_products::CurrentProductRef<'_>,
) -> Result<Vec<ResolvedNativeDependency>, CompileError> {
    let group = current.key();
    let entry = current.entry();
    let mut dependencies = Vec::new();
    for (required_owner, dependency) in entry.native_requirements_for(group.original_ordinal) {
        let ArtifactDependency::NativeGroup {
            dependent_ordinal,
            required_ordinal,
        } = dependency
        else {
            continue;
        };
        let edge = NativeGroupEdge {
            dependent_ordinal: *dependent_ordinal,
            required_ordinal: *required_ordinal,
        };
        let wanted = entry
            .native_owners
            .get(required_owner)
            .ok_or_else(|| failure("current native requirement lacks exact owner"))?;
        let missing = || {
            admission_failure(ArtifactInventoryFailure::MissingDependency {
                artifact: group.artifact,
                dependent: entry.descriptor.owner.clone(),
                required: required_owner.clone(),
                dependency: dependency.clone(),
            })
        };
        let mut current_child: Option<(NativeGroupKey, Arc<ArtifactEntry>)> = None;
        let mut known_payload = None;
        for candidate in entries.values().chain(incoming.values()) {
            let ArtifactPayload::Original(product) = &candidate.payload else {
                continue;
            };
            if NativeOwnerKey::from_owner(product.owner()) != *wanted {
                continue;
            }
            // Bytes can identify a missing grant for diagnostics; they cannot
            // authorize this dependency or substitute an input owner below.
            known_payload = Some(candidate.descriptor.id);
            let key = NativeGroupKey {
                artifact: candidate.descriptor.id,
                original_ordinal: *required_ordinal,
            };
            if current
                .dependency_product(key, candidate)
                .map_err(|error| CompileError::CompilerEvidence(Box::new(error)))?
                .is_none()
            {
                continue;
            }
            if current_child
                .as_ref()
                .is_some_and(|(previous, _)| *previous != key)
            {
                return Err(admission_failure(
                    ArtifactInventoryFailure::BindingAmbiguity {
                        artifact: key.artifact,
                    },
                ));
            }
            current_child = Some((key, Arc::clone(candidate)));
        }
        if let Some((key, candidate)) = current_child {
            let child = current
                .dependency_product(key, &candidate)
                .map_err(|error| CompileError::CompilerEvidence(Box::new(error)))?
                .expect("same cohort checked above");
            if let Some(binding) = verified_current_product(&child)? {
                let support = child
                    .own_group_support(binding)
                    .ok_or_else(|| failure("current child lost its own issued support"))?;
                let support = NativeSupportRef::checked(state, parent, binding, support)?;
                let issued = support.describe(binding)?;
                support.acquire(state, entries, incoming, &issued)?;
                if let Some(offer) = support.offer {
                    offers.push(offer.clone());
                }
                dependencies.push(ResolvedNativeDependency {
                    required_owner: required_owner.clone(),
                    edge,
                    target: NativeGroupTarget::CurrentIssued(Arc::new(issued)),
                });
            } else {
                dependencies.push(ResolvedNativeDependency {
                    required_owner: required_owner.clone(),
                    edge,
                    target: NativeGroupTarget::NewGroup(key),
                });
            }
            continue;
        }
        let unavailable = || match known_payload {
            Some(artifact) => admission_failure(ArtifactInventoryFailure::NativeGroupUnavailable {
                artifact,
                original_ordinal: *required_ordinal,
            }),
            None => missing(),
        };
        let source = current.request_inputs().ok_or_else(unavailable)?;
        let catalog = NativeDependencyCatalog::from_source(source)?;
        let mut row: Option<&Arc<crate::declaration_context::NativeTransportAvailability>> = None;
        for candidate in source.native_availability().rows() {
            if NativeOwnerKey::from_owner(candidate.original_owner()) != *wanted {
                continue;
            }
            if row.is_some_and(|previous| {
                previous.original_artifact_id() != candidate.original_artifact_id()
            }) {
                return Err(admission_failure(
                    ArtifactInventoryFailure::BindingAmbiguity {
                        artifact: candidate.original_artifact_id(),
                    },
                ));
            }
            row = Some(candidate);
        }
        let row = row.ok_or_else(unavailable)?;
        let key = NativeGroupKey {
            artifact: row.original_artifact_id(),
            original_ordinal: *required_ordinal,
        };
        let supports = catalog.group_supports(key, row.original_owner());
        let choice = match catalog.decision(required_owner) {
            InputNativeDecision::IssuedChoices(choices) => choices.get(required_ordinal).copied(),
            InputNativeDecision::UnissuedPolicy => None,
        };
        let mut supports = supports
            .into_iter()
            .filter(|(binding, _)| choice.is_none_or(|chosen| chosen == *binding));
        let Some((binding, offer)) = supports.next() else {
            return Err(admission_failure(
                ArtifactInventoryFailure::NativeGroupUnavailable {
                    artifact: key.artifact,
                    original_ordinal: key.original_ordinal,
                },
            ));
        };
        if supports.next().is_some() {
            return Err(admission_failure(
                ArtifactInventoryFailure::BindingAmbiguity {
                    artifact: key.artifact,
                },
            ));
        }
        let support = NativeSupportRef::checked(
            state,
            parent,
            binding,
            crate::certified_products::CertifiedNativeGroupSupport::Issued(offer),
        )?;
        let mut issued = support.describe(binding)?;
        if let Some(selected) = catalog.selected_support(required_owner, binding) {
            issued.source = NativeSourceCause::Historical {
                selected_at: issued.binding,
                support: selected.clone(),
            };
        }
        support.acquire(state, entries, incoming, &issued)?;
        offers.push(offer.clone());
        dependencies.push(ResolvedNativeDependency {
            required_owner: required_owner.clone(),
            edge,
            target: NativeGroupTarget::ExactGroup(Arc::new(issued)),
        });
    }
    Ok(dependencies)
}

/// One exact group's authenticated acquisition owner. Immutable graph facts
/// may be shared globally; payload custody always comes from this owner.
struct NativeSupportRef<'a> {
    snapshot: &'a crate::exact_context::ExactSelectionSnapshot,
    payloads: &'a imbl::OrdMap<ArtifactId, Arc<ArtifactEntry>>,
    offer: Option<&'a IssuedNativeOffer>,
}

impl<'a> NativeSupportRef<'a> {
    fn checked(
        state: &InventoryState,
        parent: &'a ArtifactView,
        group: NativeGroupBinding,
        support: crate::certified_products::CertifiedNativeGroupSupport<'a>,
    ) -> Result<Self, CompileError> {
        match support {
            crate::certified_products::CertifiedNativeGroupSupport::Issued(offer) => {
                if !offer.contains_checked_group(group) {
                    return Err(failure("issued native contribution lacks demanded group"));
                }
                Ok(Self {
                    snapshot: offer.custody_snapshot(),
                    payloads: &offer.custody_lease().payloads,
                    offer: Some(offer),
                })
            }
            crate::certified_products::CertifiedNativeGroupSupport::NewProductDependency(proof) => {
                validate_issued_selection(
                    state,
                    &parent.read_projection(state).nodes,
                    proof.selection.graph_selection(),
                )?;
                if !proof
                    .selection
                    .graph_selection()
                    .native_groups
                    .contains(&group)
                {
                    return Err(failure("native dependency is outside its product proof"));
                }
                Ok(Self {
                    snapshot: &parent.lease.selection,
                    payloads: &parent.lease.payloads,
                    offer: None,
                })
            }
        }
    }

    fn outgoing(
        &self,
        source: InventoryNodeKey,
    ) -> impl Iterator<Item = (InventoryNodeKey, ArtifactDependency)> + '_ {
        let rows = self
            .snapshot
            .scopes
            .get(&source.selection())
            .into_iter()
            .flat_map(move |scope| {
                let rows = &scope.proof().dependencies;
                let start = rows.partition_point(|(from, _, _)| *from < source);
                rows[start..]
                    .iter()
                    .take_while(move |(from, _, _)| *from == source)
            });
        rows.map(|(_, target, edge)| (*target, edge.clone()))
    }

    fn closure(
        &self,
        group: NativeGroupBinding,
    ) -> Result<BTreeSet<InventoryNodeKey>, CompileError> {
        self.snapshot
            .nodes_from_seeds([InventoryNodeKey::Group(group)])
            .map_err(|_| failure("issued native dependency closure missing"))
    }

    fn carrier_pair(
        &self,
        group: NativeGroupBinding,
        canonical: ArtifactId,
    ) -> Result<(ArtifactBinding, ArtifactBinding), CompileError> {
        let unique = |bindings: BTreeSet<ArtifactBinding>, artifact| {
            if bindings.len() != 1 {
                return Err(admission_failure(
                    ArtifactInventoryFailure::BindingAmbiguity { artifact },
                ));
            }
            Ok(*bindings.first().unwrap())
        };
        let carrier = unique(
            self.outgoing(InventoryNodeKey::Group(group))
                .filter_map(|(target, edge)| match target {
                    InventoryNodeKey::Artifact(binding)
                        if binding.artifact == group.binding.artifact()
                            && edge == ArtifactDependency::Interface =>
                    {
                        Some(binding)
                    }
                    _ => None,
                })
                .collect(),
            group.binding.artifact(),
        )?;
        let canonical = unique(
            self.outgoing(InventoryNodeKey::Artifact(carrier))
                .filter_map(|(target, edge)| match target {
                    InventoryNodeKey::Artifact(binding)
                        if binding.artifact == canonical
                            && edge == ArtifactDependency::Interface =>
                    {
                        Some(binding)
                    }
                    _ => None,
                })
                .collect(),
            canonical,
        )?;
        Ok((carrier, canonical))
    }

    fn describe(&self, group: NativeGroupBinding) -> Result<IssuedNativeBinding, CompileError> {
        let entry = self
            .payloads
            .get(&group.binding.artifact())
            .ok_or_else(|| failure("native support lacks its original payload"))?;
        let ArtifactPayload::Original(product) = &entry.payload else {
            return Err(failure("native support is not Original"));
        };
        let canonical = ArtifactEntry::canonical_artifact_id(product.module_interface());
        let (carrier, canonical) = self.carrier_pair(group, canonical)?;
        Ok(IssuedNativeBinding {
            binding: group,
            carrier,
            canonical,
            closure: self.closure(group)?,
            source: self
                .offer
                .cloned()
                .map(|offer| NativeSourceCause::Historical {
                    selected_at: group,
                    support: compiler_projection::SelectedNativeSupport::Acquired(Arc::from([
                        offer,
                    ])),
                })
                .unwrap_or(NativeSourceCause::Unwitnessed),
        })
    }

    fn acquire(
        &self,
        state: &mut InventoryState,
        entries: &impl ArtifactEntryMap<ArtifactId>,
        incoming: &mut BTreeMap<ArtifactId, Arc<ArtifactEntry>>,
        issued: &IssuedNativeBinding,
    ) -> Result<(), CompileError> {
        let mut pending = issued
            .closure
            .iter()
            .map(|node| node.selection())
            .collect::<Vec<_>>();
        let mut scopes = BTreeMap::new();
        while let Some(scope) = pending.pop() {
            if scopes.contains_key(&scope) {
                continue;
            }
            let proof = self
                .snapshot
                .scopes
                .get(&scope)
                .ok_or_else(|| failure("demanded closure lacks issued scope"))?
                .proof();
            pending.extend(proof.scope_dependencies());
            scopes.insert(scope, Arc::clone(proof));
        }
        let prepared = scope_proof::prepare_install(state, scopes.into_values())?;
        scope_proof::install(state, prepared);
        if admitted_closure(state, [InventoryNodeKey::Group(issued.binding)].into_iter())
            .iter()
            .copied()
            .collect::<BTreeSet<_>>()
            != issued.closure
        {
            return Err(failure(
                "issued native closure differs from its acquisition owner",
            ));
        }
        self.retain_payloads(entries, incoming, &issued.closure)
    }

    fn retain_payloads(
        &self,
        entries: &impl ArtifactEntryMap<ArtifactId>,
        incoming: &mut BTreeMap<ArtifactId, Arc<ArtifactEntry>>,
        owned: &BTreeSet<InventoryNodeKey>,
    ) -> Result<(), CompileError> {
        for node in owned {
            let InventoryNodeKey::Artifact(binding) = node else {
                continue;
            };
            let payload = self.payloads.get(&binding.artifact).ok_or_else(|| {
                failure("issued native dependency payload missing from its owner")
            })?;
            let retained = match incoming
                .get(&binding.artifact)
                .or_else(|| entries.get(&binding.artifact))
            {
                Some(previous) => ArtifactEntry::merge_catalog_input_custody(previous, payload)?,
                None => Arc::clone(payload),
            };
            incoming.insert(binding.artifact, retained);
        }
        Ok(())
    }
}

pub(super) fn issued_carrier_pair(
    parent: &ArtifactView,
    group: NativeGroupBinding,
    canonical: ArtifactId,
) -> Result<(ArtifactBinding, ArtifactBinding), CompileError> {
    let support = NativeSupportRef {
        snapshot: &parent.lease.selection,
        payloads: &parent.lease.payloads,
        offer: None,
    };
    support.closure(group)?;
    support.carrier_pair(group, canonical)
}

pub(super) fn acquired_carrier_pair(
    parent: &ArtifactView,
    group: NativeGroupBinding,
    canonical: ArtifactId,
) -> Result<(ArtifactBinding, ArtifactBinding), CompileError> {
    let state = parent.lease.inventory.0.lock().expect("inventory lock");
    let owned = &parent.read_projection(&state).nodes;
    if !owned.contains(&InventoryNodeKey::Group(group)) {
        return Err(failure("native carrier query lacks exact acquired group"));
    }
    let support = NativeSupportRef {
        snapshot: &parent.lease.selection,
        payloads: &parent.lease.payloads,
        offer: None,
    };
    let pair = support.carrier_pair(group, canonical)?;
    if !owned.contains(&InventoryNodeKey::Artifact(pair.0))
        || !owned.contains(&InventoryNodeKey::Artifact(pair.1))
    {
        return Err(failure("native carrier pair is outside acquired custody"));
    }
    Ok(pair)
}

pub(super) fn bind_certified_input_demand(
    state: &mut InventoryState,
    parent: &ArtifactView,
    entries: &impl ArtifactEntryMap<ArtifactId>,
    mut incoming: BTreeMap<ArtifactId, Arc<ArtifactEntry>>,
    demanded: &BTreeSet<NativeGroupKey>,
    selection: &crate::certified_products::CertifiedSourceSelection,
) -> Result<CertifiedNativeDemandAcquisition, CompileError> {
    enum RootIssuer {
        Historical(compiler_projection::SelectedNativeSupport),
        Current,
    }
    let namespace = parent.namespace();
    let mut retained_offers = Vec::new();
    let mut prebound = BTreeMap::new();
    let mut pending = demanded.iter().copied().collect::<Vec<_>>();
    while let Some(key) = pending.pop() {
        if prebound.contains_key(&key) {
            continue;
        }
        let key = &key;
        let entry = incoming
            .get(&key.artifact)
            .or_else(|| entries.get(&key.artifact))
            .ok_or_else(|| {
                admission_failure(ArtifactInventoryFailure::NativeRootOutsideView {
                    artifact: key.artifact,
                })
            })?
            .clone();
        let ArtifactPayload::Original(product) = &entry.payload else {
            return Err(failure("native demand lacks original payload"));
        };
        let owner = product.owner();
        let (chosen, support, issuer) = {
            if !selection.permits_native_group(key.artifact, owner, key.original_ordinal) {
                tracing::debug!(target: "tidepool::artifact_inventory::ambiguity_trace",
                        stage = "input_demand_permission", ?key, ?owner,
                        permitted = false, body_present = entry.native_group_ordinals.contains(&key.original_ordinal),
                        selected_carrier = ?namespace.get(&key.artifact),
                        "native input demand exceeds issued permission");
                return Err(native_admission_failure(
                    *key,
                    NativeAdmissionRefusal::InputPermissionDenied {
                        body_present: entry.native_group_ordinals.contains(&key.original_ordinal),
                        selected_original: namespace.get(&key.artifact).copied(),
                    },
                ));
            }
            let carried = selection
                .continuation_input(&module_identity(owner))
                .map_err(|error| CompileError::CompilerEvidence(Box::new(error)))?;
            if let Some((source, _)) = carried {
                let chosen = verified_root_choice(selection, *key, owner, &entry)?;
                let selected = source
                    .compiler_roles()
                    .selected_native_support(&module_identity(owner), chosen);
                let support = selected
                    .into_iter()
                    .flat_map(|selected| selected.acquisitions())
                    .find(|offer| offer.contains_checked_group(chosen))
                    .map(crate::certified_products::CertifiedNativeGroupSupport::Issued)
                    .ok_or_else(|| {
                        native_admission_failure(
                            *key,
                            NativeAdmissionRefusal::InputIssuedSupportMissing { candidate: chosen },
                        )
                    })?;
                (
                    chosen,
                    support,
                    RootIssuer::Historical(
                        selected
                            .cloned()
                            .unwrap_or_else(compiler_projection::SelectedNativeSupport::missing),
                    ),
                )
            } else {
                let current = selection
                    .current_product(*key, &entry)
                    .map_err(|error| CompileError::CompilerEvidence(Box::new(error)))?
                    .ok_or_else(|| {
                        native_admission_failure(
                            *key,
                            NativeAdmissionRefusal::CurrentProductNotCompleted {
                                original: key.artifact,
                                original_ordinal: key.original_ordinal,
                            },
                        )
                    })?;
                crate::declaration_context::validate_current_stage_native_inputs(
                    &current, entries,
                )?;
                let Some(chosen) = verified_current_product(&current)? else {
                    let dependencies = current_product_dependencies(
                        state,
                        parent,
                        entries,
                        &mut incoming,
                        &mut retained_offers,
                        &current,
                    )?;
                    pending.extend(dependencies.iter().filter_map(|dependency| {
                        match dependency.target() {
                            NativeGroupTarget::NewGroup(group) => Some(*group),
                            NativeGroupTarget::ExactGroup(_)
                            | NativeGroupTarget::CurrentIssued(_) => None,
                        }
                    }));
                    prebound.insert(*key, VerifiedNativeBinding::NewCurrent { dependencies });
                    continue;
                };
                let support = current.own_group_support(chosen).ok_or_else(|| {
                    native_admission_failure(
                        *key,
                        NativeAdmissionRefusal::InputIssuedSupportMissing { candidate: chosen },
                    )
                })?;
                (chosen, support, RootIssuer::Current)
            }
        };
        let canonical = ArtifactEntry::canonical_artifact_id(product.module_interface());
        let support = NativeSupportRef::checked(state, parent, chosen, support)?;
        let mut issued = support.describe(chosen)?;
        if let RootIssuer::Historical(selected) = &issuer {
            issued.source = NativeSourceCause::Historical {
                selected_at: issued.binding,
                support: selected.clone(),
            };
        }
        let carrier = issued.carrier;
        let canonical_binding = issued.canonical;
        // An interface-only receiving role may retain the issued body's
        // unchanged historical canonical binding without selecting Original.
        let historical_interface = support.offer.is_some()
            && !namespace.contains_key(&key.artifact)
            && selection.interface_only_artifact(&entry.descriptor.owner) == Some(canonical);
        let carrier_matches = namespace
            .get(&key.artifact)
            .is_none_or(|selected| *selected == carrier);
        let canonical_matches = namespace
            .get(&canonical)
            .is_none_or(|selected| *selected == canonical_binding || historical_interface);
        if !carrier_matches || !canonical_matches {
            tracing::debug!(target: "tidepool::artifact_inventory::ambiguity_trace",
                stage = "input_demand_carrier", ?key, owner = ?product.owner(),
                selected_carrier = ?namespace.get(&key.artifact),
                ?canonical, selected_canonical = ?namespace.get(&canonical),
                "issued native input binding does not match selected carrier or canonical binding");
            return Err(native_admission_failure(
                *key,
                NativeAdmissionRefusal::InputNoCompatibleCandidate {
                    selected_original: namespace.get(&key.artifact).copied(),
                    canonical,
                    selected_canonical: namespace.get(&canonical).copied(),
                },
            ));
        }
        support.acquire(state, entries, &mut incoming, &issued)?;
        if let Some(offer) = support.offer {
            retained_offers.push(offer.clone());
        }
        prebound.insert(
            *key,
            match issuer {
                RootIssuer::Historical(_) => VerifiedNativeBinding::Issued(issued),
                RootIssuer::Current => VerifiedNativeBinding::CurrentIssued(issued),
            },
        );
    }
    Ok(CertifiedNativeDemandAcquisition {
        entries: incoming,
        bindings: prebound,
        offers: retained_offers,
    })
}

/// Carried source inputs cannot issue a fresh physical product. New bodies
/// enter through the completed current-cohort issuer instead.
pub(super) fn bind_fresh_product_dependencies(
    _state: &mut InventoryState,
    _parent: &ArtifactView,
    _consumed_namespace: &BTreeMap<ExactModuleIdentity, Arc<ArtifactEntry>>,
    _entries: &impl ArtifactEntryMap<ArtifactId>,
    fresh: &BTreeSet<NativeGroupKey>,
    _catalog: &NativeDependencyCatalog<'_>,
) -> Result<CertifiedNativeDemandAcquisition, CompileError> {
    if let Some(group) = fresh.iter().next() {
        return Err(native_admission_failure(
            *group,
            NativeAdmissionRefusal::CurrentProductNotCompleted {
                original: group.artifact,
                original_ordinal: group.original_ordinal,
            },
        ));
    }
    Ok(CertifiedNativeDemandAcquisition {
        entries: BTreeMap::new(),
        bindings: BTreeMap::new(),
        offers: Vec::new(),
    })
}

impl CertifiedNativeDemandAcquisition {
    fn validate_dependency_namespace(
        &self,
        entries: &impl ArtifactEntryMap<ArtifactId>,
        consumed_namespace: &BTreeMap<ExactModuleIdentity, Arc<ArtifactEntry>>,
    ) -> Result<(), CompileError> {
        for binding in self.bindings.values() {
            let closures = match binding {
                VerifiedNativeBinding::Issued(issued)
                | VerifiedNativeBinding::CurrentIssued(issued) => vec![&issued.closure],
                VerifiedNativeBinding::NewCurrent { dependencies } => dependencies
                    .iter()
                    .filter_map(|dependency| match dependency.target() {
                        NativeGroupTarget::ExactGroup(issued)
                        | NativeGroupTarget::CurrentIssued(issued) => Some(&issued.closure),
                        NativeGroupTarget::NewGroup(_) => None,
                    })
                    .collect(),
            };
            for node in closures.into_iter().flatten() {
                let InventoryNodeKey::Group(group) = node else {
                    continue;
                };
                let entry = self
                    .entries
                    .get(&group.binding.artifact())
                    .or_else(|| entries.get(&group.binding.artifact()))
                    .ok_or_else(|| failure("acquired native dependency payload missing"))?;
                validate_consumed_native_namespace(entry, consumed_namespace)?;
            }
        }
        Ok(())
    }
}

/// Consuming exact native support must respect a selected compiler role. An
/// absent role grants no namespace intent; an incompatible selected role cannot
/// be bypassed by retaining a historical implementation beside it.
#[track_caller]
pub(crate) fn validate_consumed_native_namespace(
    dependency: &ArtifactEntry,
    namespace: &impl super::ArtifactEntryMap<ExactModuleIdentity>,
) -> Result<(), CompileError> {
    let ArtifactPayload::Original(product) = &dependency.payload else {
        return Err(failure("issued native dependency is not an original"));
    };
    let Some(selected) = namespace.get(&dependency.descriptor.owner) else {
        return Ok(());
    };
    let canonical = match &selected.payload {
        ArtifactPayload::Canonical(interface) => Some(interface),
        ArtifactPayload::Original(product) => Some(product.module_interface()),
        ArtifactPayload::Interface(..) | ArtifactPayload::PackageCode(_) => None,
    };
    if canonical != Some(product.module_interface())
        || (matches!(selected.payload, ArtifactPayload::Original(_))
            && selected.descriptor.id != dependency.descriptor.id)
    {
        tracing::warn!(
            target: "tidepool::artifact_inventory::namespace_conflict",
            owner = ?dependency.descriptor.owner,
            dependency = ?dependency.descriptor.id,
            selected = ?selected.descriptor.id,
            selected_kind = ?selected.descriptor.kind,
            interfaces_match = canonical == Some(product.module_interface()),
            caller = %std::panic::Location::caller(),
            "issued native dependency conflicts with request interface"
        );
        return Err(failure(
            "issued native dependency conflicts with request interface",
        ));
    }
    Ok(())
}

/// Durable offers retain only the exact reachable causes frozen at issuance.
/// Keep the operation's persistent map outside this recursive acquisition DAG.
#[derive(Clone, Debug)]
pub(super) struct FrozenNativeSourceReceipts {
    entries: Box<[(NativeSourceUse, Arc<[NativeSourceCause]>)]>,
}

impl FrozenNativeSourceReceipts {
    pub(super) fn get(&self, key: &NativeSourceUse) -> Option<&Arc<[NativeSourceCause]>> {
        self.entries
            .binary_search_by_key(key, |(key, _)| *key)
            .ok()
            .map(|index| &self.entries[index].1)
    }
}

#[derive(Clone, Debug)]
enum NativeOfferSourceReceipt {
    Solved(Arc<FrozenNativeSourceReceipts>),
    Unwitnessed,
}

#[derive(Clone)]
/// Exact lifetime custody, without a derived artifact-view projection.
/// Only the inventory's checked issuance or an already authenticated view can
/// construct this owner; immutable proof bytes alone are insufficient.
struct NativeOfferCustody {
    lease: Arc<ViewLease>,
    reachable: imbl::OrdSet<ArtifactBindingNode>,
    native_originals: BTreeSet<ArtifactId>,
    artifact_carriers: BTreeSet<ArtifactBinding>,
    required_scopes: BTreeSet<ArtifactSelectionId>,
    source_receipt: NativeOfferSourceReceipt,
}

impl NativeOfferCustody {
    fn from_checked_nodes(
        lease: Arc<ViewLease>,
        reachable: imbl::OrdSet<ArtifactBindingNode>,
        source_receipt: &IssuedNativeSourceReceipt,
        source_roots: &BTreeMap<u32, NativeGroupBinding>,
    ) -> Result<Self, CompileError> {
        let mut native_originals = BTreeSet::new();
        let mut artifact_carriers = BTreeSet::new();
        let mut required_scopes = BTreeSet::new();
        let mut retained_sources = BTreeMap::new();
        let state = lease.inventory.0.lock().expect("inventory lock");
        for node in &reachable {
            let group = match node {
                ArtifactBindingNode::Artifact(carrier) => {
                    artifact_carriers.insert(*carrier);
                    required_scopes.insert(carrier.selection);
                    continue;
                }
                ArtifactBindingNode::Group(group) => group,
            };
            required_scopes.insert(group.binding.selection());
            if let IssuedNativeSourceReceipt::Solved(sources) = source_receipt {
                let root = NativeSourceUse::Root(*group);
                if let Some(causes) = sources
                    .0
                    .get(&root)
                    .filter(|_| source_roots.get(&group.original_ordinal) == Some(group))
                {
                    retained_sources.insert(root, Arc::clone(causes));
                }
                let first = NativeSourceUse::Edge {
                    parent: *group,
                    child: NativeGroupBinding {
                        binding: NativeGroupIssuer {
                            artifact: ArtifactId([0; 32]),
                            selection: ArtifactSelectionId([0; 32]),
                        },
                        original_ordinal: 0,
                    },
                };
                for (key, causes) in sources.0.range(first..) {
                    let NativeSourceUse::Edge { parent, child } = key else {
                        break;
                    };
                    if parent != group {
                        break;
                    }
                    if reachable.contains(&ArtifactBindingNode::Group(*child)) {
                        retained_sources.insert(*key, Arc::clone(causes));
                    }
                }
            }
            let mut carriers = state
                .outgoing(*node)
                .filter_map(|(target, edge)| match target {
                    ArtifactBindingNode::Artifact(carrier)
                        if carrier.artifact == group.binding.artifact()
                            && *edge == ArtifactDependency::Interface =>
                    {
                        Some(carrier)
                    }
                    _ => None,
                });
            let carrier = carriers
                .next()
                .ok_or_else(|| failure("issued native group has no carrier"))?;
            if carriers.next().is_some() {
                return Err(admission_failure(
                    ArtifactInventoryFailure::BindingAmbiguity {
                        artifact: carrier.artifact,
                    },
                ));
            }
            if !reachable.contains(&ArtifactBindingNode::Artifact(carrier)) {
                return Err(failure("issued native carrier is outside checked closure"));
            }
            native_originals.insert(group.binding.artifact());
        }
        drop(state);
        // Availability is already checked. Capture only its physical facts,
        // without holding the inventory lock or retaining broad source caches.
        crate::host_work::checkpoint()?;
        let artifacts = artifact_ids(&reachable)
            .into_iter()
            .map(|id| {
                lease
                    .payloads
                    .get(&id)
                    .cloned()
                    .map(|entry| (id, entry))
                    .ok_or_else(|| {
                        failure("native offer physical payload is outside checked custody")
                    })
            })
            .collect::<Result<BTreeMap<_, _>, _>>()?;
        let available_groups = reachable
            .iter()
            .filter_map(|node| match node {
                ArtifactBindingNode::Group(group) => Some(*group),
                _ => None,
            })
            .collect();
        let mut materializations = Vec::new();
        lease.collect_materializations(&mut materializations, &mut BTreeSet::new())?;
        let physical = crate::declaration_context::RetainedArtifactMaterialization::capture_custody(
            &materializations,
            &artifacts,
            &available_groups,
        );
        let lease = Arc::new(ViewLease {
            admission_roots: lease.admission_roots.clone(),
            selection: Arc::clone(&lease.selection),
            payloads: lease.payloads.clone(),
            acquired_owner_counts: lease.acquired_owner_counts.clone(),
            inventory: lease.inventory.clone(),
            materialization_sources: Default::default(),
            materialization: MaterializationSource {
                cache: Arc::new(Mutex::new(BTreeMap::new())),
                parents: Arc::from([]),
                fixed: physical.into_iter().collect(),
            },
        });
        Ok(Self {
            lease,
            reachable,
            native_originals,
            artifact_carriers,
            required_scopes,
            source_receipt: match source_receipt {
                IssuedNativeSourceReceipt::Solved(_) => {
                    NativeOfferSourceReceipt::Solved(Arc::new(FrozenNativeSourceReceipts {
                        entries: retained_sources.into_iter().collect(),
                    }))
                }
                IssuedNativeSourceReceipt::Unwitnessed => NativeOfferSourceReceipt::Unwitnessed,
            },
        })
    }
}

impl std::fmt::Debug for NativeOfferCustody {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeOfferCustody")
            .field("roots", &self.lease.selection.roots)
            .field("custody", &self.lease.selection.custody_roots)
            .finish()
    }
}

/// One availability issuance and exactly the graph custody supporting it.
/// Propagation can retain or narrow this owner, never rediscover another proof.
#[derive(Clone, Debug)]
pub(crate) struct IssuedNativeOffer(Arc<NativeOfferFacts>);

impl IssuedNativeOffer {
    pub(crate) fn same_recorded_facts(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0) || self.snapshot_facts() == other.snapshot_facts()
    }
}

/// The exact portion of an admitted offer consumed by one request. Its
/// membership remains tied to that offer's proof and acquisition lifetimes.
#[derive(Clone, Debug)]
pub(crate) struct NativeOfferContribution {
    offer: IssuedNativeOffer,
    reachable: imbl::OrdSet<ArtifactBindingNode>,
    originals: BTreeSet<ArtifactId>,
    identity: OnceLock<[u8; 32]>,
}

impl NativeOfferContribution {
    pub(crate) fn same_recorded_facts(&self, other: &Self) -> bool {
        self.offer.same_recorded_facts(&other.offer)
            && self.reachable == other.reachable
            && self.originals == other.originals
    }
}

#[cfg(test)]
thread_local! {
    static GROUP_BINDING_SCOPE_VISITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

impl NativeOfferContribution {
    pub(crate) fn same_acquisition(&self, other: &Self) -> bool {
        self.offer.same_issuance(&other.offer)
            && self.reachable == other.reachable
            && self.originals == other.originals
    }

    pub(crate) fn group_carrier_pair(
        &self,
        group: NativeGroupBinding,
        canonical: ArtifactId,
    ) -> Result<(ArtifactBinding, ArtifactBinding), CompileError> {
        if !self.contains_group(group) {
            return Err(failure("native carrier query exceeds its contribution"));
        }
        let support = NativeSupportRef {
            snapshot: &self.offer.0.custody.lease.selection,
            payloads: &self.offer.0.custody.lease.payloads,
            offer: Some(&self.offer),
        };
        let pair = support.carrier_pair(group, canonical)?;
        if !self.reachable.contains(&InventoryNodeKey::Artifact(pair.0))
            || !self.reachable.contains(&InventoryNodeKey::Artifact(pair.1))
        {
            return Err(failure("native carrier pair exceeds its contribution"));
        }
        Ok(pair)
    }

    #[cfg(test)]
    pub(crate) fn take_group_binding_scope_visits() -> usize {
        GROUP_BINDING_SCOPE_VISITS.with(|work| work.replace(0))
    }

    pub(crate) fn matches_direct_canonical_binding(
        &self,
        original: ArtifactId,
        canonical: ArtifactId,
        namespace: &ArtifactView,
    ) -> bool {
        self.offer
            .matches_canonical_binding_within(original, canonical, namespace, &self.reachable)
    }

    pub(crate) fn snapshot_facts(
        &self,
    ) -> Result<crate::declaration_context::NativeContributionSnapshotFacts, CompileError> {
        Ok(
            crate::declaration_context::NativeContributionSnapshotFacts {
                offer: self.offer.snapshot_identity()?,
                reachable: self.reachable.clone(),
            },
        )
    }

    pub(crate) fn identity(&self) -> Result<[u8; 32], CompileError> {
        if let Some(identity) = self.identity.get() {
            return Ok(*identity);
        }
        let identity = self.snapshot_facts()?.identity()?;
        let _ = self.identity.set(identity);
        Ok(identity)
    }

    pub(crate) fn restore(
        offer: IssuedNativeOffer,
        facts: &crate::declaration_context::NativeContributionSnapshotFacts,
    ) -> Result<Self, CompileError> {
        if offer.snapshot_identity()? != facts.offer
            || !facts
                .reachable
                .iter()
                .all(|node| offer.0.custody.reachable.contains(node))
        {
            return Err(failure("native contribution exceeds its issued proof"));
        }
        let state = offer
            .0
            .custody
            .lease
            .inventory
            .0
            .lock()
            .expect("inventory lock");
        let closure = admitted_closure(&state, facts.reachable.iter().copied());
        if !closure.iter().all(|node| facts.reachable.contains(node)) {
            return Err(failure("native contribution omits an issued dependency"));
        }
        drop(state);
        let originals = facts
            .reachable
            .iter()
            .filter_map(|node| match node {
                ArtifactBindingNode::Group(group) => Some(group.binding.artifact()),
                _ => None,
            })
            .collect();
        let identity = OnceLock::new();
        let _ = identity.set(facts.identity()?);
        Ok(Self {
            offer,
            reachable: facts.reachable.clone(),
            originals,
            identity,
        })
    }

    /// Restrict an already issued contribution to authenticated dependency roots.
    pub(crate) fn for_groups(
        self: &Arc<Self>,
        roots: &BTreeSet<NativeGroupBinding>,
    ) -> Result<Arc<Self>, CompileError> {
        Ok(self.restrict_groups(roots, None, None)?.0)
    }

    pub(crate) fn for_groups_with_operation(
        self: &Arc<Self>,
        roots: &BTreeSet<NativeGroupBinding>,
        operation: &tidepool_repr::execution_schema::InventoryOperation,
    ) -> Result<(Arc<Self>, NativeClosureWork), CompileError> {
        self.restrict_groups(roots, Some(operation), None)
    }

    /// Resolve retained values while walking this same exact dependency closure.
    /// Their identities are read from authenticated binding edges, not a worker
    /// capture list or the surrounding inventory.
    pub(crate) fn for_groups_with_runtime_requirements(
        self: &Arc<Self>,
        roots: &BTreeSet<NativeGroupBinding>,
        operation: &tidepool_repr::execution_schema::InventoryOperation,
    ) -> Result<(Arc<Self>, Vec<NativeBindingRequirement>), CompileError> {
        let mut retained = BTreeSet::new();
        let (closure, _) = self.restrict_groups(roots, Some(operation), Some(&mut retained))?;
        operation
            .reserve::<NativeBindingRequirement>(retained.len())
            .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
        Ok((closure, retained.into_iter().collect()))
    }

    fn restrict_groups(
        self: &Arc<Self>,
        roots: &BTreeSet<NativeGroupBinding>,
        operation: Option<&tidepool_repr::execution_schema::InventoryOperation>,
        mut retained: Option<&mut BTreeSet<NativeBindingRequirement>>,
    ) -> Result<(Arc<Self>, NativeClosureWork), CompileError> {
        let state = self
            .offer
            .0
            .custody
            .lease
            .inventory
            .0
            .lock()
            .expect("inventory lock");
        let (reachable, work) = admitted_closure_observing(
            &state,
            roots
                .iter()
                .copied()
                .filter(|group| self.reachable.contains(&ArtifactBindingNode::Group(*group)))
                .map(ArtifactBindingNode::Group),
            operation,
            |_, target, edge| {
                let Some(retained) = retained.as_deref_mut() else {
                    return Ok(());
                };
                let ArtifactDependency::NativeBinding {
                    generation,
                    namespace,
                    occurrence,
                    record_parent,
                    ..
                } = edge
                else {
                    return Ok(());
                };
                let descriptor =
                    &self.offer.0.custody.lease.payloads[&target.artifact()].descriptor;
                if let Some(operation) = operation {
                    operation
                        .reserve::<NativeBindingRequirement>(1)
                        .and_then(|_| {
                            operation.charge(
                                descriptor.owner.unit.len()
                                    + descriptor.owner.module.len()
                                    + namespace.len()
                                    + occurrence.len()
                                    + record_parent.as_ref().map_or(0, String::len),
                            )
                        })
                        .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
                }
                retained.insert(NativeBindingRequirement {
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
                Ok(())
            },
        )?;
        for node in &reachable {
            if operation.is_some() {
                crate::host_work::checkpoint()?;
            }
            if !self.reachable.contains(node) {
                return Err(failure("restricted native contribution exceeds its issuer"));
            }
        }
        drop(state);
        if reachable == self.reachable {
            return Ok((Arc::clone(self), work));
        }
        let mut originals = BTreeSet::new();
        for node in &reachable {
            if let ArtifactBindingNode::Group(group) = node {
                if operation.is_some() {
                    crate::host_work::checkpoint()?;
                }
                if !originals.contains(&group.binding.artifact()) {
                    if let Some(operation) = operation {
                        operation
                            .reserve::<(ArtifactId, [usize; 4])>(1)
                            .map_err(|error| {
                                CompileError::CompilerEvidence(Box::new(error.into()))
                            })?;
                    }
                    originals.insert(group.binding.artifact());
                }
            }
        }
        if let Some(operation) = operation {
            operation
                .reserve::<Self>(1)
                .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
        }
        let restricted = Arc::new(Self {
            offer: self.offer.clone(),
            reachable,
            originals,
            identity: OnceLock::new(),
        });
        if let Some(operation) = operation {
            // A changed contribution's durable identity encodes only this
            // actual closure. Reserve its node vector and bounded CBOR tuples
            // before the existing identity encoder allocates either buffer.
            operation
                .reserve::<[ArtifactBindingNode; 2]>(restricted.reachable.len())
                .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
            operation
                .reserve::<[u8; 256]>(restricted.reachable.len().saturating_add(1))
                .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
            crate::host_work::checkpoint()?;
            restricted.identity()?;
        }
        Ok((restricted, work))
    }

    /// Borrow this contribution's exact groups for one original, without
    /// enumerating the offer's retained selection scopes.
    pub(crate) fn original_groups(
        &self,
        original: ArtifactId,
    ) -> impl Iterator<Item = NativeGroupBinding> + '_ {
        self.reachable
            .range(
                ArtifactBindingNode::Group(NativeGroupBinding {
                    binding: NativeGroupIssuer {
                        artifact: original,
                        selection: ArtifactSelectionId([0; 32]),
                    },
                    original_ordinal: 0,
                })..=ArtifactBindingNode::Group(NativeGroupBinding {
                    binding: NativeGroupIssuer {
                        artifact: original,
                        selection: ArtifactSelectionId([u8::MAX; 32]),
                    },
                    original_ordinal: u32::MAX,
                }),
            )
            .filter_map(move |node| match node {
                ArtifactBindingNode::Group(group) if group.binding.artifact() == original => {
                    Some(*group)
                }
                _ => None,
            })
    }

    /// HOME-specific observation through the contribution's custody lease.
    /// PackageCode custody under the same owner name is not an Original.
    pub(crate) fn has_home_original_owner(&self, owner: &ExactModuleIdentity) -> bool {
        self.offer().custody_lease().has_home_acquired_owner(owner)
    }

    pub(crate) fn original_group_bindings(
        &self,
        operation: &tidepool_repr::execution_schema::InventoryOperation,
    ) -> Result<Vec<NativeGroupBinding>, CompileError> {
        operation
            .reserve::<NativeGroupBinding>(self.reachable.len())
            .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
        Ok(self
            .reachable
            .iter()
            .filter_map(|node| match node {
                ArtifactBindingNode::Group(group) => Some(*group),
                _ => None,
            })
            .collect())
    }

    pub(crate) fn original_group_children(
        &self,
        group: NativeGroupBinding,
        operation: &tidepool_repr::execution_schema::InventoryOperation,
    ) -> Result<Vec<NativeGroupBinding>, CompileError> {
        let lease = self.offer().custody_lease();
        let state = lease.inventory.0.lock().expect("inventory lock");
        original_group_children_in_scope(&state, &self.reachable, group, operation)
    }

    pub(crate) fn original_group_retained_binding(
        &self,
        group: NativeGroupBinding,
        identity: &tidepool_repr::execution_schema::SymbolIdentity,
        generation: u64,
        operation: &tidepool_repr::execution_schema::InventoryOperation,
    ) -> Result<ArtifactBinding, CompileError> {
        let lease = self.offer().custody_lease();
        let state = lease.inventory.0.lock().expect("inventory lock");
        original_group_retained_binding_in_scope(
            &state,
            &lease.payloads,
            &self.reachable,
            group,
            identity,
            generation,
            operation,
        )
    }

    pub(crate) fn contains_group(&self, group: NativeGroupBinding) -> bool {
        self.reachable.contains(&ArtifactBindingNode::Group(group))
    }

    pub(crate) fn offer(&self) -> &IssuedNativeOffer {
        &self.offer
    }

    pub(crate) fn originals(&self) -> &BTreeSet<ArtifactId> {
        &self.originals
    }

    pub(crate) fn groups(
        &self,
        key: NativeGroupKey,
    ) -> impl Iterator<Item = NativeGroupBinding> + '_ {
        self.original_groups(key.artifact)
            .filter(move |group| group.original_ordinal == key.original_ordinal)
    }

    pub(crate) fn permits(&self, key: NativeGroupKey) -> bool {
        self.groups(key).next().is_some()
    }

    pub(crate) fn ordinals(&self, original: ArtifactId) -> impl Iterator<Item = u32> + '_ {
        self.original_groups(original)
            .map(|group| group.original_ordinal)
    }
}

#[derive(Debug)]
struct NativeOfferFacts {
    original_acquisition: Arc<ArtifactEntry>,
    original: ArtifactId,
    interface: ArtifactId,
    owner: ExactModuleIdentity,
    body: NativeOfferBody,
    custody: NativeOfferCustody,
    snapshot: OnceLock<crate::exact_context::NativeOfferSnapshotFacts>,
    identity: OnceLock<[u8; 32]>,
}

#[derive(Debug, PartialEq, Eq)]
enum NativeOfferBody {
    Empty,
    Partial(NativeOriginalRequirement),
    Full {
        proof: NativeOriginalRequirement,
        qualification: QualifiedNativeAvailability,
    },
}

/// The scope solver has checked exact body rows. The product validator must
/// accept the complete census before these offers can escape this owner.
pub(crate) struct CheckedNativeAvailability {
    offers: Vec<IssuedNativeOffer>,
    entries: BTreeMap<ArtifactId, Arc<ArtifactEntry>>,
    roots: BTreeSet<ArtifactId>,
}

/// Fresh producer roots and their complete offers have passed body and
/// dependency validation together. Retained entries cannot add issuance roots.
pub(crate) struct ValidatedFreshNativeAvailability {
    roots: BTreeSet<ArtifactId>,
    offers: Vec<IssuedNativeOffer>,
}

impl ValidatedFreshNativeAvailability {
    pub(crate) fn roots(&self) -> &BTreeSet<ArtifactId> {
        &self.roots
    }

    pub(crate) fn offers(&self) -> &[IssuedNativeOffer] {
        &self.offers
    }

    pub(crate) fn into_offers(self) -> Vec<IssuedNativeOffer> {
        self.offers
    }
}

impl CheckedNativeAvailability {
    pub(super) fn validate_current_cohort(
        self,
        validation: &mut crate::recovery_artifacts::PackageInterfaceValidation,
    ) -> Result<ValidatedFreshNativeAvailability, CompileError> {
        let evidence = |error| CompileError::CompilerEvidence(Box::new(error));
        let operation = Arc::clone(&validation.inventory);
        operation
            .reserve::<NativeOfferContribution>(self.offers.len())
            .map_err(|error| evidence(error.into()))?;
        let mut contributions = Vec::with_capacity(self.offers.len());
        let mut available = BTreeMap::new();
        for offer in &self.offers {
            crate::host_work::checkpoint()?;
            let contribution = offer
                .contribution_for(&BTreeSet::from([offer.original()]))?
                .ok_or_else(|| failure("cohort offer lost its own contribution"))?;
            for id in contribution.originals() {
                let entry = self
                    .entries
                    .get(id)
                    .ok_or_else(|| failure("cohort proof lost an exact original payload"))?;
                let ArtifactPayload::Original(product) = &entry.payload else {
                    return Err(failure("cohort native proof has non-original payload"));
                };
                if !available.contains_key(id) {
                    operation
                        .reserve::<(
                            ArtifactId,
                            &crate::recovery_artifacts::CertifiedRecoveryProduct,
                            [usize; 4],
                        )>(1)
                        .map_err(|error| evidence(error.into()))?;
                    product
                        .original_native()
                        .validate_package_interfaces(validation)
                        .map_err(evidence)?;
                    available.insert(*id, product);
                }
            }
            contributions.push(contribution);
        }
        let mut inventory = crate::certified_products::OriginalValidationInventory::selected(
            &available, &operation,
        )
        .map_err(evidence)?;
        for contribution in &contributions {
            inventory
                .validate_selected_scope(
                    crate::certified_products::SelectedOriginalGroupScope::Contribution(
                        contribution,
                    ),
                    validation,
                )
                .map_err(evidence)?;
        }
        Ok(ValidatedFreshNativeAvailability {
            roots: self.roots,
            offers: self.offers,
        })
    }

    pub(super) fn from_complete_custody(
        admitted: IssuedArtifactCustody,
        originals: &[ArtifactId],
    ) -> Result<Self, CompileError> {
        let entries = admitted.entries.clone();
        let offers = IssuedNativeOffer::issue_custody(admitted, originals)?;
        if offers
            .iter()
            .any(|offer| matches!(offer.0.body, NativeOfferBody::Partial(_)))
        {
            return Err(failure(
                "complete availability lacks its issued group census",
            ));
        }
        Ok(Self {
            offers,
            entries,
            roots: originals.iter().copied().collect(),
        })
    }

    pub(super) fn from_cohort_custody(
        admitted: IssuedArtifactCustody,
        originals: &[ArtifactId],
    ) -> Result<Self, CompileError> {
        let entries = admitted.entries.clone();
        Ok(Self {
            offers: IssuedNativeOffer::issue_custody(admitted, originals)?,
            entries,
            roots: originals.iter().copied().collect(),
        })
    }
}

impl IssuedNativeOffer {
    /// Resolve this selected occurrence through its own immutable custody.
    /// The anchor remains unchanged while historical descendants are followed.
    fn source_causes_at(
        &self,
        selected_at: NativeGroupBinding,
        requested: NativeGroupBinding,
    ) -> Result<Arc<[NativeSourceCause]>, CompileError> {
        let custody = &self.0.custody;
        if !custody
            .reachable
            .contains(&ArtifactBindingNode::Group(selected_at))
        {
            return Err(failure(
                "native source anchor is outside its selected offer",
            ));
        }
        let issued = match &custody.source_receipt {
            NativeOfferSourceReceipt::Solved(sources) => NativeSourceReceiptView::Frozen(sources),
            NativeOfferSourceReceipt::Unwitnessed => NativeSourceReceiptView::Unwitnessed,
        };
        let mut sources = NativeSourceTraversal {
            issued,
            selected: NativeSourceReceipts::default(),
        };
        let state = custody.lease.inventory.0.lock().expect("inventory lock");
        let reached = native_groups_in_custody(
            &state,
            &custody.lease.payloads,
            &custody.reachable,
            vec![selected_at],
            None,
            Some(&mut sources),
        )?;
        if !reached.contains(&requested) {
            return Err(failure(
                "native source group is outside its selected anchor",
            ));
        }
        Ok(Arc::clone(
            sources
                .selected
                .0
                .get(&NativeSourceUse::Root(requested))
                .expect("a reached group retains its source cause"),
        ))
    }

    /// Complete availability is issued directly by the checked scope solver.
    /// A complete census in bytes alone cannot enter this constructor.
    fn issue_custody(
        admitted: IssuedArtifactCustody,
        originals: &[ArtifactId],
    ) -> Result<Vec<Self>, CompileError> {
        let source_receipt = admitted.source_receipt;
        let supplied = admitted.supplied;
        let source = admitted.lease;
        let issuing_view = ArtifactView::from_custody(Arc::clone(&source));
        let mut offers = Vec::new();
        let state = source.inventory.0.lock().expect("inventory lock");
        // Index the solver-issued roots once. The carrier is part of the key:
        // equal payloads from different issuances must not share a choice.
        let requested_originals = originals.iter().copied().collect::<BTreeSet<_>>();
        let mut issued_roots =
            BTreeMap::<(ArtifactId, ArtifactBinding), BTreeMap<u32, NativeGroupBinding>>::new();
        let mut ambiguous_roots = BTreeSet::new();
        for root in &source.admission_roots {
            let ArtifactBindingNode::Group(group) = root else {
                continue;
            };
            if !requested_originals.contains(&group.binding.artifact()) {
                continue;
            }
            for (target, edge) in state.outgoing(*root) {
                let ArtifactBindingNode::Artifact(carrier) = target else {
                    continue;
                };
                if *edge != ArtifactDependency::Interface
                    || carrier.artifact != group.binding.artifact()
                {
                    continue;
                }
                let prior = issued_roots
                    .entry((group.binding.artifact(), carrier))
                    .or_default()
                    .insert(group.original_ordinal, *group);
                if prior.is_some_and(|prior| prior != *group) {
                    ambiguous_roots.insert((group.binding.artifact(), carrier));
                }
            }
        }
        for &original in originals {
            let outcome = supplied.get(&original).ok_or_else(|| {
                failure("native availability lacks its supplied artifact outcome")
            })?;
            let entry = &outcome.entry;
            let ArtifactPayload::Original(product) = &entry.payload else {
                return Err(admission_failure(
                    ArtifactInventoryFailure::NativeRootNotOriginal { artifact: original },
                ));
            };
            let interface = ArtifactEntry::canonical_artifact_id(product.module_interface());
            let carrier = outcome.binding;
            let mut canonical = state
                .outgoing(ArtifactBindingNode::Artifact(carrier))
                .filter_map(|(node, edge)| match node {
                    ArtifactBindingNode::Artifact(binding)
                        if binding.artifact == interface
                            && *edge == ArtifactDependency::Interface =>
                    {
                        Some(binding)
                    }
                    _ => None,
                });
            let selected_canonical = canonical
                .next()
                .ok_or_else(|| failure("native availability lacks its supplied canonical edge"))?;
            if canonical.next().is_some() {
                return Err(failure(
                    "native availability has competing supplied canonical edges",
                ));
            }
            let canonical = selected_canonical;
            if ambiguous_roots.contains(&(original, carrier)) {
                return Err(admission_failure(
                    ArtifactInventoryFailure::BindingAmbiguity { artifact: original },
                ));
            }
            let roots = issued_roots
                .remove(&(original, carrier))
                .unwrap_or_default();
            let complete = entry
                .native_group_ordinals
                .iter()
                .all(|ordinal| roots.contains_key(ordinal));
            if roots
                .keys()
                .any(|ordinal| !entry.native_group_ordinals.contains(ordinal))
            {
                return Err(failure(
                    "native availability exceeds its supplied group census",
                ));
            }
            let selected_roots = if roots.is_empty() {
                vec![ArtifactBindingNode::Artifact(carrier)]
            } else {
                roots
                    .values()
                    .copied()
                    .map(ArtifactBindingNode::Group)
                    .collect()
            };
            let executable_nodes = admitted_closure(&state, selected_roots.iter().copied())
                .into_iter()
                .collect::<BTreeSet<_>>();
            let owned = &issuing_view.read_projection(&state).nodes;
            let metadata_nodes =
                issuing_view.native_payload_custody_nodes(&state, owned, &executable_nodes)?;
            // Nominal body references retain their exact carrier and TYPE
            // dependencies as custody. Close those metadata seeds before
            // creating a lease whose snapshot can traverse their edges.
            let nodes = admitted_closure(&state, metadata_nodes.iter().copied());
            if nodes.iter().any(|node| !owned.contains(node)) {
                return Err(failure(
                    "native offer metadata closure leaves its issuing custody",
                ));
            }
            let metadata = nodes
                .iter()
                .filter(|node| !executable_nodes.contains(node))
                .copied()
                .collect::<Vec<_>>();
            let selected = selection_snapshot_for_lease(
                &state,
                &BTreeMap::from([(original, carrier), (interface, canonical)]),
                &selected_roots,
                &metadata,
                &[],
            );
            let groups = executable_nodes
                .iter()
                .filter_map(|node| match node {
                    ArtifactBindingNode::Group(group) => Some(*group),
                    _ => None,
                })
                .collect();
            let mut custody_selection = (*selected).clone();
            for (root, ()) in &custody_selection.roots {
                custody_selection.custody_roots.insert(*root, ());
            }
            custody_selection.roots.clear();
            custody_selection.namespace.clear();
            let mut payloads = Default::default();
            let mut acquired_owner_counts = Default::default();
            for id in artifact_ids(&nodes) {
                ViewLease::insert_payload(
                    &mut payloads,
                    &mut acquired_owner_counts,
                    id,
                    Arc::clone(source.payloads.get(&id).ok_or_else(|| {
                        failure("native offer metadata closure lacks its issued payload")
                    })?),
                );
            }
            let lease = Arc::new(ViewLease {
                admission_roots: Default::default(),
                selection: Arc::new(custody_selection),
                payloads,
                acquired_owner_counts,
                inventory: source.inventory.clone(),
                materialization_sources: source.materialization_sources.clone(),
                materialization: source.materialization.clone(),
            });
            let body = if roots.is_empty() {
                NativeOfferBody::Empty
            } else if !complete {
                NativeOfferBody::Partial(NativeOriginalRequirement {
                    original,
                    selection: ExactArtifactSelection::from_snapshot(selected),
                })
            } else {
                NativeOfferBody::Full {
                    proof: NativeOriginalRequirement {
                        original,
                        selection: ExactArtifactSelection::from_snapshot(selected),
                    },
                    qualification: QualifiedNativeAvailability {
                        original,
                        carrier,
                        canonical,
                        groups,
                    },
                }
            };
            offers.push((
                original,
                interface,
                entry.descriptor.owner.clone(),
                Arc::clone(entry),
                body,
                lease,
                nodes,
                roots,
            ));
        }
        drop(state);
        offers
            .into_iter()
            .map(
                |(original, interface, owner, original_acquisition, body, lease, nodes, roots)| {
                    Ok(Self(Arc::new(NativeOfferFacts {
                        original_acquisition,
                        original,
                        interface,
                        owner,
                        body,
                        custody: NativeOfferCustody::from_checked_nodes(
                            lease,
                            nodes,
                            &source_receipt,
                            &roots,
                        )?,
                        snapshot: OnceLock::new(),
                        identity: OnceLock::new(),
                    })))
                },
            )
            .collect()
    }

    pub(crate) fn full_qualification(&self) -> Option<&QualifiedNativeAvailability> {
        match &self.0.body {
            NativeOfferBody::Full { qualification, .. } => Some(qualification),
            _ => None,
        }
    }

    /// The same immutable issuance also retains the same acquisition owners.
    /// Semantic offer equality alone does not establish this relationship.
    pub(crate) fn same_issuance(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }

    /// Issuance is restricted to an input owner's view before independent
    /// inputs merge. `None` issues empty availability rather than discovery.
    pub(super) fn issue(
        input: CompilerPrimaryInput<'_>,
        proof: Option<NativeOriginalRequirement>,
    ) -> Result<Self, CompileError> {
        let original_binding = input.original().ok_or_else(|| {
            admission_failure(ArtifactInventoryFailure::MissingOriginal {
                owner: input.owner().clone(),
            })
        })?;
        let canonical_binding = input.interface();
        let entry = input.entry();
        let source = input.artifacts();
        let original = original_binding.artifact;
        let ArtifactPayload::Original(_) = &entry.payload else {
            return Err(admission_failure(
                ArtifactInventoryFailure::NativeRootNotOriginal { artifact: original },
            ));
        };
        let interface = canonical_binding.artifact;
        let (body, custody) = match proof {
            None => {
                let binding = original_binding;
                let custody = source.inventory().retain(
                    Vec::new(),
                    vec![ArtifactBindingNode::Artifact(binding)],
                    Vec::new(),
                    Vec::new(),
                );
                let mut custody = source.retain_projected_materializations(custody)?;
                custody.replace_namespace(&BTreeMap::new());
                (NativeOfferBody::Empty, custody)
            }
            Some(proof) => {
                // Group-root proofs deliberately omit a compiler namespace.
                // Their authenticated root/carrier/canonical edges still name
                // the exact acquired row; namespace presence is not required.
                let pair_matches = {
                    let state = source.lease.inventory.0.lock().expect("inventory lock");
                    let graph = proof.selection.graph_selection();
                    proof.namespace_pair_in(&state, graph, canonical_binding.artifact)
                        == Some((original_binding, canonical_binding))
                        && graph.roots.iter().all(|root| match root {
                            InventoryNodeKey::Group(group)
                                if group.binding.artifact() == original =>
                            {
                                state
                                    .outgoing(*root)
                                    .filter(|(_, edge)| **edge == ArtifactDependency::Interface)
                                    .filter_map(|(target, _)| match target {
                                        InventoryNodeKey::Artifact(binding)
                                            if binding.artifact == original =>
                                        {
                                            Some(binding)
                                        }
                                        _ => None,
                                    })
                                    .all(|binding| binding == original_binding)
                            }
                            _ => true,
                        })
                };
                if proof.original != original || !pair_matches {
                    return Err(admission_failure(
                        ArtifactInventoryFailure::NativeOfferBindingMismatch {
                            owner: input.owner().clone(),
                            original: original_binding,
                            interface: canonical_binding,
                        },
                    ));
                }
                let qualification = proof.qualify_availability(source)?;
                let graph = proof.selection.graph_selection();
                let custody = source.inventory().retain(
                    Vec::new(),
                    graph
                        .roots
                        .iter()
                        .chain(&graph.custody_roots)
                        .copied()
                        .collect(),
                    Vec::new(),
                    Vec::new(),
                );
                let mut custody = source.retain_projected_materializations(custody)?;
                custody.replace_namespace(&BTreeMap::new());
                // Proof roots remain issuer facts in `proof`; retaining their
                // nodes here grants neither compiler roles nor execution demand.
                proof.issued_groups(&custody)?;
                let body = match qualification {
                    Some(qualification) => NativeOfferBody::Full {
                        proof,
                        qualification,
                    },
                    None => NativeOfferBody::Partial(proof),
                };
                (body, custody)
            }
        };
        let reachable = {
            let state = custody.lease.inventory.0.lock().expect("inventory lock");
            custody.read_projection(&state).nodes.clone()
        };
        let custody = NativeOfferCustody::from_checked_nodes(
            Arc::clone(&custody.lease),
            reachable,
            &IssuedNativeSourceReceipt::Unwitnessed,
            &BTreeMap::new(),
        )?;
        Ok(Self(Arc::new(NativeOfferFacts {
            original_acquisition: Arc::clone(entry),
            original,
            interface,
            owner: entry.descriptor.owner.clone(),
            body,
            custody,
            snapshot: OnceLock::new(),
            identity: OnceLock::new(),
        })))
    }

    /// Borrow durable custody facts without constructing a selected view.
    /// Reopening these facts still requires the recovery admission boundary.
    pub(crate) fn custody_snapshot(&self) -> &Arc<crate::exact_context::ExactSelectionSnapshot> {
        &self.0.custody.lease.selection
    }

    pub(crate) fn snapshot_facts(&self) -> crate::exact_context::NativeOfferSnapshotFacts {
        use crate::exact_context::{NativeOfferSnapshotBody, NativeOfferSnapshotFacts};
        self.0
            .snapshot
            .get_or_init(|| NativeOfferSnapshotFacts {
                original: self.0.original,
                interface: self.0.interface,
                owner: self.0.owner.clone(),
                body: match &self.0.body {
                    NativeOfferBody::Empty => NativeOfferSnapshotBody::Empty(
                        ExactArtifactSelection::from_snapshot(Arc::clone(self.custody_snapshot())),
                    ),
                    NativeOfferBody::Partial(proof) => {
                        NativeOfferSnapshotBody::Partial(proof.selection.clone())
                    }
                    NativeOfferBody::Full { proof, .. } => {
                        NativeOfferSnapshotBody::Full(proof.selection.clone())
                    }
                },
            })
            .clone()
    }

    pub(crate) fn snapshot_identity(&self) -> Result<[u8; 32], CompileError> {
        if let Some(identity) = self.0.identity.get() {
            return Ok(*identity);
        }
        let identity = self.snapshot_facts().identity()?;
        let _ = self.0.identity.set(identity);
        Ok(identity)
    }

    /// Restore the recorded availability class against authenticated recovered
    /// custody. Successful qualification cannot upgrade a partial issuance.
    pub(crate) fn restore_snapshot(
        facts: &crate::exact_context::NativeOfferSnapshotFacts,
        source: &ArtifactView,
    ) -> Result<Self, CompileError> {
        use crate::exact_context::NativeOfferSnapshotBody;
        let selection = match &facts.body {
            NativeOfferSnapshotBody::Empty(selection)
            | NativeOfferSnapshotBody::Partial(selection)
            | NativeOfferSnapshotBody::Full(selection) => selection,
        };
        let snapshot = selection.snapshot();
        if snapshot
            .scopes
            .keys()
            .any(|scope| !source.lease.selection.scopes.contains_key(scope))
        {
            return Err(failure(
                "recovered native offer scope is outside acquisition custody",
            ));
        }
        let graph = snapshot
            .graph_selection()
            .map_err(|_| failure("recovered native offer has malformed exact closure"))?;
        let nodes = graph
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
        if matches!(&facts.body, NativeOfferSnapshotBody::Empty(_))
            && (!graph.roots.is_empty()
                || !graph.namespace.is_empty()
                || !matches!(graph.custody_roots.as_slice(), [ArtifactBindingNode::Artifact(binding)] if binding.artifact == facts.original))
        {
            return Err(failure(
                "recovered empty offer lacks exact original custody",
            ));
        }
        let mut edges = Vec::new();
        for node in &nodes {
            let proof = snapshot.scopes[&node.selection()].proof();
            let start = proof
                .dependencies
                .partition_point(|(source, _, _)| source < node);
            edges.extend(
                proof.dependencies[start..]
                    .iter()
                    .take_while(|(source, _, _)| source == node)
                    .cloned(),
            );
        }
        {
            let state = source.lease.inventory.0.lock().expect("inventory lock");
            let owned = &source.read_projection(&state).nodes;
            if graph
                .bindings
                .iter()
                .any(|binding| !owned.contains(&ArtifactBindingNode::Artifact(*binding)))
            {
                return Err(failure(
                    "recovered native offer carrier is outside acquisition custody",
                ));
            }
        }
        let entries = graph
            .bindings
            .iter()
            .map(|binding| {
                source
                    .lease
                    .payloads
                    .get(&binding.artifact)
                    .cloned()
                    .ok_or_else(|| {
                        failure("recovered native offer payload is outside acquisition custody")
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        // This is independent cold admission of the recorded rows and complete
        // proof witnesses, not authority inferred from a receiving inventory.
        let recovered = source
            .inventory()
            .recover_selected_custody(source, entries, &graph, &edges)?;
        let entry = recovered
            .entries
            .get(&facts.original)
            .ok_or_else(|| failure("recovered native offer lacks its original"))?;
        let ArtifactPayload::Original(product) = &entry.payload else {
            return Err(admission_failure(
                ArtifactInventoryFailure::NativeRootNotOriginal {
                    artifact: facts.original,
                },
            ));
        };
        let interface = ArtifactEntry::canonical_artifact_id(product.module_interface());
        if interface != facts.interface || entry.descriptor.owner != facts.owner {
            return Err(failure(
                "recovered native offer differs from its original interface or owner",
            ));
        }
        let state = recovered.lease.inventory.0.lock().expect("inventory lock");
        let body = match &facts.body {
            NativeOfferSnapshotBody::Empty(_) => NativeOfferBody::Empty,
            NativeOfferSnapshotBody::Partial(_) => {
                NativeOfferBody::Partial(NativeOriginalRequirement {
                    original: facts.original,
                    selection: selection.clone(),
                })
            }
            NativeOfferSnapshotBody::Full(_) => {
                let proof = NativeOriginalRequirement {
                    original: facts.original,
                    selection: selection.clone(),
                };
                let qualification = proof
                    .qualify_checked_lease(
                        &recovered.lease,
                        &state,
                        &graph,
                        NativeAvailabilityQualificationScope::IssuedClosure,
                    )?
                    .ok_or_else(|| failure("recovered full native offer no longer qualifies"))?;
                NativeOfferBody::Full {
                    proof,
                    qualification,
                }
            }
        };
        drop(state);
        let mut custody_selection = recovered.lease.selection.as_ref().clone();
        for root in custody_selection.roots.keys().copied().collect::<Vec<_>>() {
            custody_selection.custody_roots.insert(root, ());
        }
        custody_selection.roots.clear();
        custody_selection.namespace.clear();
        let lease = ViewLease::from_issued_draft(
            ViewDraft {
                roots: vec![],
                selection: Arc::new(custody_selection),
                inventory: source.inventory().clone(),
                parents: vec![recovered.lease],
                issuing_payloads: BTreeMap::new(),
                materialization_parents: vec![],
                materialization: Mutex::new(BTreeMap::new()),
            },
            BTreeSet::new(),
        );
        let custody = NativeOfferCustody::from_checked_nodes(
            lease,
            nodes.into_iter().collect(),
            &IssuedNativeSourceReceipt::Unwitnessed,
            &BTreeMap::new(),
        )?;
        Ok(Self(Arc::new(NativeOfferFacts {
            original_acquisition: Arc::clone(entry),
            original: facts.original,
            interface,
            owner: facts.owner.clone(),
            body,
            custody,
            snapshot: OnceLock::new(),
            identity: OnceLock::new(),
        })))
    }

    pub(crate) fn original(&self) -> ArtifactId {
        self.0.original
    }

    pub(crate) fn original_acquisition(&self) -> &Arc<ArtifactEntry> {
        &self.0.original_acquisition
    }

    /// Reopen this producer's exact offered subset as future input choices.
    /// The physical cohort must retain the same direct carrier pair; broader
    /// custody and alternative offers cannot add a root to this projection.
    pub(crate) fn produced_publication_view(
        &self,
        physical: &ArtifactView,
    ) -> Result<ArtifactView, CompileError> {
        if !Arc::ptr_eq(
            &physical.lease.inventory.0,
            &self.0.custody.lease.inventory.0,
        ) {
            return Err(failure("produced publication belongs to another inventory"));
        }
        let carrier = physical
            .lease
            .selection
            .namespace
            .get(&self.0.original)
            .copied()
            .ok_or_else(|| failure("produced publication lacks its direct body binding"))?;
        let canonical = physical
            .lease
            .selection
            .namespace
            .get(&self.0.interface)
            .copied()
            .ok_or_else(|| failure("produced publication lacks its direct canonical binding"))?;
        if !self.matches_direct_canonical_binding(self.0.original, self.0.interface, physical) {
            return Err(failure(
                "produced publication changed its exact carrier pair",
            ));
        }
        let snapshot = if let Some(proof) = self.proof() {
            if proof.selection.snapshot().namespace.get(&self.0.original) != Some(&carrier)
                || proof.selection.snapshot().namespace.get(&self.0.interface) != Some(&canonical)
                || proof.selection.snapshot().roots.keys().any(|root| {
                    !matches!(root, ArtifactBindingNode::Group(group)
                        if group.binding.artifact() == self.0.original)
                })
            {
                return Err(failure(
                    "produced publication differs from its own offered roots",
                ));
            }
            Arc::clone(proof.selection.snapshot())
        } else {
            let mut snapshot = (*self.0.custody.lease.selection).clone();
            snapshot.namespace.insert(self.0.original, carrier);
            snapshot.namespace.insert(self.0.interface, canonical);
            snapshot
                .roots
                .insert(ArtifactBindingNode::Artifact(carrier), ());
            Arc::new(snapshot)
        };
        // Passive physical views retain offer owners without selecting their
        // groups. Reopen only this offer's actual checked custody, not a union
        // of available graphs in the physical namespace.
        let custody = ArtifactView::from_custody(Arc::clone(&self.0.custody.lease));
        custody.select_issued(
            vec![self.0.original, self.0.interface],
            &ExactArtifactSelection::from_snapshot(snapshot),
        )
    }

    pub(crate) fn proof(&self) -> Option<&NativeOriginalRequirement> {
        match &self.0.body {
            NativeOfferBody::Empty => None,
            NativeOfferBody::Partial(proof) | NativeOfferBody::Full { proof, .. } => Some(proof),
        }
    }

    pub(crate) fn is_full(&self) -> bool {
        matches!(self.0.body, NativeOfferBody::Full { .. })
    }

    pub(super) fn custody_lease(&self) -> &Arc<ViewLease> {
        &self.0.custody.lease
    }

    pub(super) fn artifact_carriers(&self) -> &BTreeSet<ArtifactBinding> {
        &self.0.custody.artifact_carriers
    }

    /// Scopes of actual checked artifact carriers and group issuers. A group
    /// can be issued separately from the scope owning its carrier edge.
    pub(crate) fn required_scopes(&self) -> &BTreeSet<ArtifactSelectionId> {
        &self.0.custody.required_scopes
    }

    pub(super) fn offers_groups_for(&self, originals: &BTreeSet<ArtifactId>) -> bool {
        !self.0.custody.native_originals.is_disjoint(originals)
    }

    pub(super) fn contains_checked_group(&self, group: NativeGroupBinding) -> bool {
        self.0
            .custody
            .reachable
            .contains(&ArtifactBindingNode::Group(group))
    }

    /// Query only this issuance's checked reachable membership. Complete scope
    /// rows outside the offer's selection cannot supply a missing binding.
    pub(crate) fn group_bindings(
        &self,
        key: NativeGroupKey,
    ) -> impl Iterator<Item = NativeGroupBinding> + '_ {
        self.0
            .custody
            .lease
            .selection
            .scopes
            .keys()
            .filter_map(move |selection| {
                #[cfg(test)]
                GROUP_BINDING_SCOPE_VISITS.with(|work| work.set(work.get() + 1));
                let group = NativeGroupBinding {
                    binding: NativeGroupIssuer {
                        artifact: key.artifact,
                        selection: *selection,
                    },
                    original_ordinal: key.original_ordinal,
                };
                self.0
                    .custody
                    .reachable
                    .contains(&ArtifactBindingNode::Group(group))
                    .then_some(group)
            })
    }

    /// Consume this issuer's complete private body beside an exact type role.
    /// Body availability does not select an original or executable roots.
    pub(crate) fn qualified_private_availability(
        &self,
        roles: AdmittedCompilerRoles<'_>,
        namespace: &ArtifactView,
    ) -> Option<&QualifiedNativeAvailability> {
        self.qualification_for_role(roles.get(&self.0.owner), namespace)
    }

    /// Assembly may observe the qualification of this already-issued offer;
    /// this does not acquire a compiler row or grant an alternative body.
    pub(crate) fn qualifies_assembly(
        &self,
        roles: AssemblyRoleView<'_>,
        namespace: &ArtifactView,
    ) -> bool {
        self.qualification_for_role(roles.get(&self.0.owner), namespace)
            .is_some()
    }

    fn qualification_for_role(
        &self,
        role: Option<CompilerRoleFacts<'_>>,
        namespace: &ArtifactView,
    ) -> Option<&QualifiedNativeAvailability> {
        let NativeOfferBody::Full { qualification, .. } = &self.0.body else {
            return None;
        };
        let role = role?;
        (role.interface() == self.0.interface
            && role
                .original()
                .is_none_or(|original| original == self.0.original)
            && qualification.matches_namespace(namespace))
        .then_some(qualification)
    }

    /// Observe an issued carrier's affinity without granting any body groups.
    /// Empty and partial rows keep their normalized permission unchanged.
    pub(crate) fn matches_direct_canonical_binding(
        &self,
        original: ArtifactId,
        canonical: ArtifactId,
        namespace: &ArtifactView,
    ) -> bool {
        self.matches_canonical_binding_within(
            original,
            canonical,
            namespace,
            &self.0.custody.reachable,
        )
    }

    fn matches_canonical_binding_within(
        &self,
        original: ArtifactId,
        canonical: ArtifactId,
        namespace: &ArtifactView,
        reachable: &imbl::OrdSet<ArtifactBindingNode>,
    ) -> bool {
        let Some(canonical) = namespace.lease.selection.namespace.get(&canonical) else {
            return false;
        };
        self.0.custody.artifact_carriers.iter().any(|carrier| {
            if carrier.artifact != original
                || !reachable.contains(&ArtifactBindingNode::Artifact(*carrier))
                || namespace
                    .lease
                    .selection
                    .namespace
                    .get(&original)
                    .is_some_and(|selected| selected != carrier)
            {
                return false;
            }
            let source = ArtifactBindingNode::Artifact(*carrier);
            let proof = self
                .custody_snapshot()
                .scopes
                .get(&carrier.selection)
                .expect("checked native offer retains its carrier scope")
                .proof();
            let start = proof
                .dependencies
                .partition_point(|(from, _, _)| *from < source);
            proof.dependencies[start..]
                .iter()
                .take_while(|(from, _, _)| *from == source)
                .any(|(_, target, edge)| {
                    *target == ArtifactBindingNode::Artifact(*canonical)
                        && *edge == ArtifactDependency::Interface
                        && reachable.contains(target)
                })
        })
    }

    pub(crate) fn issued_group_carriers(
        &self,
    ) -> Result<BTreeMap<NativeGroupBinding, ArtifactBinding>, CompileError> {
        if self.proof().is_none() {
            return Ok(BTreeMap::new());
        }
        let state = self
            .0
            .custody
            .lease
            .inventory
            .0
            .lock()
            .expect("inventory lock");
        self.0
            .custody
            .reachable
            .iter()
            .filter_map(|node| match node {
                ArtifactBindingNode::Group(group) => Some(group),
                _ => None,
            })
            .map(|group| {
                let mut carriers = state
                    .outgoing(ArtifactBindingNode::Group(*group))
                    .filter_map(|(target, edge)| match target {
                        ArtifactBindingNode::Artifact(carrier)
                            if carrier.artifact == group.binding.artifact()
                                && *edge == ArtifactDependency::Interface =>
                        {
                            Some(carrier)
                        }
                        _ => None,
                    });
                let carrier = carriers
                    .next()
                    .ok_or_else(|| failure("issued native group has no carrier"))?;
                if carriers.next().is_some() {
                    return Err(admission_failure(
                        ArtifactInventoryFailure::BindingAmbiguity {
                            artifact: carrier.artifact,
                        },
                    ));
                }
                Ok((*group, carrier))
            })
            .collect()
    }

    pub(super) fn authored_binding_requirements(
        &self,
        accepted: &BTreeSet<ExactModuleIdentity>,
    ) -> Result<Vec<NativeBindingRequirement>, CompileError> {
        if !accepted.contains(&self.0.owner) {
            return Ok(Vec::new());
        }
        let lease = &self.0.custody.lease;
        if !matches!(&lease.payloads[&self.original()].payload, ArtifactPayload::Original(product)
            if matches!(product.module_interface().origin(),
                crate::certified_products::CanonicalOrigin::NativeAuthoredDeclaration { .. }))
        {
            return Ok(Vec::new());
        }
        let roots = self
            .0
            .custody
            .reachable
            .iter()
            .filter_map(|node| match node {
                ArtifactBindingNode::Group(group)
                    if group.binding.artifact() == self.original() =>
                {
                    Some(*group)
                }
                _ => None,
            })
            .collect();
        let state = lease.inventory.0.lock().expect("inventory lock");
        Ok(native_requirements_for_exact_groups_in_custody(
            &state,
            &lease.payloads,
            &self.0.custody.reachable,
            roots,
        )?
        .0
        .bindings)
    }

    /// Consume either this offer's own body or the immutable descendants of
    /// requested dependency bodies. Unrelated sibling bodies remain custody.
    pub(crate) fn contribution_for(
        &self,
        originals: &BTreeSet<ArtifactId>,
    ) -> Result<Option<NativeOfferContribution>, CompileError> {
        let reachable = if originals.contains(&self.original()) {
            self.0.custody.reachable.clone()
        } else {
            if !self
                .0
                .custody
                .native_originals
                .iter()
                .any(|id| originals.contains(id))
            {
                return Ok(None);
            }
            let state = self
                .0
                .custody
                .lease
                .inventory
                .0
                .lock()
                .expect("inventory lock");
            let nodes = admitted_closure(
                &state,
                self.0
                    .custody
                    .reachable
                    .iter()
                    .filter_map(|node| match node {
                        ArtifactBindingNode::Group(group)
                            if originals.contains(&group.binding.artifact()) =>
                        {
                            Some(*node)
                        }
                        _ => None,
                    }),
            );
            if !nodes
                .iter()
                .all(|node| self.0.custody.reachable.contains(node))
            {
                return Err(failure(
                    "native contribution exceeds its checked offer closure",
                ));
            }
            nodes
        };
        let originals = reachable
            .iter()
            .filter_map(|node| match node {
                ArtifactBindingNode::Group(group) => Some(group.binding.artifact()),
                _ => None,
            })
            .collect();
        Ok(Some(NativeOfferContribution {
            offer: self.clone(),
            reachable,
            originals,
            identity: OnceLock::new(),
        }))
    }

    /// A receipt can carry this proof for one of its dependencies. Consume that
    /// dependency's exact closure, independently of the proof's original roots.
    pub(crate) fn dependency_group_carriers(
        &self,
        originals: &BTreeSet<ArtifactId>,
    ) -> Result<BTreeMap<NativeGroupBinding, ArtifactBinding>, CompileError> {
        let groups = self.issued_group_carriers()?;
        let state = self
            .0
            .custody
            .lease
            .inventory
            .0
            .lock()
            .expect("inventory lock");
        let closure = admitted_closure(
            &state,
            groups
                .keys()
                .filter(|group| originals.contains(&group.binding.artifact()))
                .copied()
                .map(InventoryNodeKey::Group),
        );
        Ok(groups
            .into_iter()
            .filter(|(group, _)| closure.contains(&InventoryNodeKey::Group(*group)))
            .collect())
    }

    /// Source support preserves the issuer's availability while selecting only
    /// offers related to its canonical roles or exact native dependencies.
    pub(crate) fn is_relevant_to_support(
        &self,
        roles: AdmittedCompilerRoles<'_>,
    ) -> Result<bool, CompileError> {
        if roles
            .get(&self.0.owner)
            .is_some_and(|role| role.interface() == self.0.interface)
        {
            return Ok(true);
        }
        let originals = roles
            .iter()
            .filter_map(|(_, role)| role.original())
            .collect::<BTreeSet<_>>();
        Ok(self
            .0
            .custody
            .native_originals
            .iter()
            .any(|original| originals.contains(original)))
    }

    /// Current roles may stop offering an original, or stop qualifying its full
    /// census. Neither change authorizes an alternative ambient proof.
    #[track_caller]
    pub(crate) fn within_projection(
        &self,
        roles: AdmittedCompilerRoles<'_>,
        namespace: &ArtifactView,
    ) -> Result<Option<Self>, CompileError> {
        self.narrow_issued_offer(
            roles.get(&self.0.owner),
            roles.iter().filter_map(|(_, role)| role.original()),
            namespace,
        )
    }

    /// Narrow an existing issuance while a compiler namespace is assembled.
    /// Permission still comes solely from this offer's acquired custody.
    pub(crate) fn within_assembly(
        &self,
        roles: AssemblyRoleView<'_>,
        namespace: &ArtifactView,
    ) -> Result<Option<Self>, CompileError> {
        self.narrow_issued_offer(
            roles.get(&self.0.owner),
            roles.iter().filter_map(|(_, role)| role.original()),
            namespace,
        )
    }

    fn narrow_issued_offer(
        &self,
        role: Option<CompilerRoleFacts<'_>>,
        originals: impl Iterator<Item = ArtifactId>,
        namespace: &ArtifactView,
    ) -> Result<Option<Self>, CompileError> {
        if self.qualification_for_role(role, namespace).is_some() {
            return Ok(Some(self.clone()));
        }
        let originals = originals.collect::<BTreeSet<_>>();
        let contributes = self
            .0
            .custody
            .native_originals
            .iter()
            .any(|original| originals.contains(original));
        let Some(original) = role.and_then(CompilerRoleFacts::original) else {
            return Ok(contributes.then(|| self.clone()));
        };
        let role = role.expect("native role");
        if original != self.0.original || role.interface() != self.0.interface {
            if contributes {
                return Ok(Some(self.clone()));
            }
            return Err(admission_failure(
                ArtifactInventoryFailure::NativeOfferSelectionConflict {
                    owner: self.0.owner.clone(),
                    offered_original: self.0.original,
                    offered_interface: self.0.interface,
                    selected_original: original,
                    selected_interface: role.interface(),
                },
            ));
        }
        let NativeOfferBody::Full {
            proof,
            qualification,
        } = &self.0.body
        else {
            return Ok(Some(self.clone()));
        };
        if qualification.matches_namespace(namespace) {
            return Ok(Some(self.clone()));
        }
        Ok(Some(Self(Arc::new(NativeOfferFacts {
            original_acquisition: Arc::clone(&self.0.original_acquisition),
            original: self.0.original,
            interface: self.0.interface,
            owner: self.0.owner.clone(),
            body: NativeOfferBody::Partial(proof.clone()),
            custody: self.0.custody.clone(),
            snapshot: OnceLock::new(),
            identity: OnceLock::new(),
        }))))
    }
}

#[cfg(test)]
pub(super) fn assert_actual_emitted_dependency_policy(
    available: &CompilerSourceSupport,
    missing: &CompilerSourceSupport,
    parent: &ArtifactEntry,
    helper: &ArtifactEntry,
    expected_group: NativeGroupBinding,
    expected_offer: &IssuedNativeOffer,
) {
    assert!(parent
        .native_requirements
        .values()
        .flatten()
        .any(|(owner, edge)| {
            owner == &helper.descriptor.owner
                && matches!(edge, ArtifactDependency::NativeGroup { required_ordinal, .. }
                if *required_ordinal == expected_group.original_ordinal)
        }));
    let ArtifactPayload::Original(product) = &helper.payload else {
        panic!("the dependency is an actual completed Original");
    };
    let key = NativeGroupKey {
        artifact: helper.descriptor.id,
        original_ordinal: expected_group.original_ordinal,
    };
    assert_eq!(expected_group.binding.artifact(), key.artifact);
    assert_eq!(
        parent.native_owners.get(&helper.descriptor.owner),
        Some(&NativeOwnerKey::from_owner(product.owner())),
        "the emitted edge names this actual dependency's native owner"
    );
    for source in [available, missing] {
        // Recovery may retain another acquisition for the same exact group.
        // This premise requires the actual Full offer to remain available;
        // it does not choose a winning contribution from their stored order.
        assert!(source
            .native_availability()
            .custody_contributions()
            .iter()
            .any(|(artifact, contribution)| {
                *artifact == key.artifact
                    && contribution.contains_group(expected_group)
                    && contribution.offer().same_issuance(expected_offer)
                    && contribution.offer().is_full()
            }));
    }
    let catalog = NativeDependencyCatalog::from_source(available).unwrap();
    assert!(matches!(catalog.decision(&helper.descriptor.owner),
        InputNativeDecision::IssuedChoices(choices) if choices.is_empty()));
    let supports = catalog.group_supports(key, product.owner());
    assert_eq!(supports.len(), 1);
    assert_eq!(supports[0].0, expected_group);
    assert!(supports[0].1.same_issuance(expected_offer));
    assert!(expected_offer.is_full());

    let catalog = NativeDependencyCatalog::from_source(missing).unwrap();
    assert!(matches!(catalog.decision(&helper.descriptor.owner),
        InputNativeDecision::IssuedChoices(choices)
            if choices.get(&expected_group.original_ordinal) == Some(&expected_group)));
    assert!(matches!(
        catalog.selected_support(&helper.descriptor.owner, expected_group),
        Some(SelectedNativeSupport::Missing(_))
    ));
    assert!(missing
        .native_offers()
        .iter()
        .any(|offer| offer.same_issuance(expected_offer)));
    assert!(catalog.group_supports(key, product.owner()).is_empty(),
        "an actual emitted dependency cannot repair a selected ordinal's missing acquisition from Full availability");
}

#[cfg(test)]
mod normalized_dependency_tests {
    use super::*;
    use crate::certified_products::tests::{
        original_groups_fixture_with_interface, recovered_witness_fixtures,
    };
    use crate::certified_products::{fixture_finalized_product, PendingImportOwner};
    use crate::declaration_context::{ExactCompileContext, ExactDeclarationContext};

    #[test]
    fn complete_availability_preserves_only_its_own_solver_source_receipt() {
        let producer = CanonicalProducerIdentity::from_producer_bytes(b"private-source-receipt");
        let entries = [("ReceiptLeft", 7, 23), ("ReceiptRight", 11, 31)]
            .into_iter()
            .map(|(module, ordinal, seed)| {
                let product = fixture_finalized_product(
                    original_groups_fixture_with_interface(
                        module,
                        vec![(ordinal, vec![])],
                        seed,
                        &BTreeMap::new(),
                        format!("{module} interface").into_bytes(),
                    ),
                    producer.sha256(),
                );
                Arc::new(ArtifactEntry::original(producer.sha256(), product).unwrap())
            })
            .collect::<Vec<_>>();
        let inventory = ArtifactInventory::default();
        let physical = inventory
            .admit_shared_with_demand(
                &inventory.empty_view(),
                entries.clone(),
                NativeArtifactDemand::ScopeInterfaces,
            )
            .unwrap();
        let projection = CompilerInputProjection::from_issued_entries(&entries).unwrap();
        let mut validation = crate::recovery_artifacts::PackageInterfaceValidation::default();
        let mut selection =
            crate::certified_products::CertifiedSourceSelection::from_compiler_projection(
                &projection,
                &physical.metadata_snapshot(),
                &validation.inventory,
            )
            .unwrap();
        let stage = crate::declaration_context::CapturedCompilerStage::issue(
            &mut selection,
            &physical,
            entries,
            &mut validation,
        )
        .unwrap();
        let inputs = stage.compiler_inputs();
        assert_eq!(inputs.issued_offers().len(), 2);
        for offer in inputs.issued_offers() {
            let NativeOfferSourceReceipt::Solved(sources) = &offer.0.custody.source_receipt else {
                panic!("actual complete availability lost its solver issuer");
            };
            assert_eq!(
                sources.entries.len(),
                1,
                "an unrelated sibling receipt is not retained"
            );
            let (NativeSourceUse::Root(group), causes) = sources.entries.iter().next().unwrap()
            else {
                panic!("the leaf product has exactly its own root receipt");
            };
            assert_eq!(group.binding.artifact(), offer.original());
            assert!(matches!(causes.as_ref(), [NativeSourceCause::Current]));
            let contribution = Arc::new(
                offer
                    .contribution_for(&BTreeSet::from([offer.original()]))
                    .unwrap()
                    .unwrap(),
            );
            let narrowed = contribution.for_groups(&BTreeSet::from([*group])).unwrap();
            let NativeOfferSourceReceipt::Solved(retained) =
                &narrowed.offer().0.custody.source_receipt
            else {
                panic!("narrowing cannot discard an actual issuer");
            };
            assert!(Arc::ptr_eq(sources, retained));

            let custody = ArtifactView::from_custody(Arc::clone(offer.custody_lease()));
            let graph = custody
                .select_issued(vec![offer.original()], &offer.proof().unwrap().selection)
                .unwrap();
            let graph_only =
                IssuedNativeOffer::restore_snapshot(&offer.snapshot_facts(), &graph).unwrap();
            assert!(
                matches!(
                    graph_only.0.custody.source_receipt,
                    NativeOfferSourceReceipt::Unwitnessed
                ),
                "authenticated graph facts cannot recreate an actual source issuer"
            );
        }
    }

    #[test]
    fn historical_source_anchors_keep_selected_parent_paths_distinct() {
        let producer = CanonicalProducerIdentity::from_producer_bytes(b"source-anchor-paths");
        let child = fixture_finalized_product(
            original_groups_fixture_with_interface(
                "SourceAnchorChild",
                vec![(13, vec![])],
                41,
                &BTreeMap::new(),
                b"anchor child interface".to_vec(),
            ),
            producer.sha256(),
        );
        let dependency = PendingImportOwner::Source {
            owner: child.owner().clone(),
            original_ordinal: 13,
            binder: tidepool_repr::execution_schema::testing::identity(
                "SourceAnchorChild",
                "entry_13",
            ),
        };
        let parent = fixture_finalized_product(
            original_groups_fixture_with_interface(
                "SourceAnchorParent",
                vec![(8, vec![dependency.clone()]), (11, vec![dependency])],
                43,
                &BTreeMap::new(),
                b"anchor parent interface".to_vec(),
            ),
            producer.sha256(),
        );
        let child = Arc::new(ArtifactEntry::original(producer.sha256(), child).unwrap());
        let parent = Arc::new(ArtifactEntry::original(producer.sha256(), parent).unwrap());
        let entries = vec![child.clone(), parent.clone()];
        let inventory = ArtifactInventory::default();
        let physical = inventory
            .admit_shared_with_demand(
                &inventory.empty_view(),
                entries.clone(),
                NativeArtifactDemand::ScopeInterfaces,
            )
            .unwrap();
        let projection = CompilerInputProjection::from_issued_entries(&entries).unwrap();
        let mut validation = crate::recovery_artifacts::PackageInterfaceValidation::default();
        let mut selection =
            crate::certified_products::CertifiedSourceSelection::from_compiler_projection(
                &projection,
                &physical.metadata_snapshot(),
                &validation.inventory,
            )
            .unwrap();
        let stage = crate::declaration_context::CapturedCompilerStage::issue(
            &mut selection,
            &physical,
            entries,
            &mut validation,
        )
        .unwrap();
        let inputs = stage.compiler_inputs();
        let offer = inputs
            .issued_offers()
            .iter()
            .find(|offer| offer.original() == parent.descriptor.id)
            .unwrap();
        let binding = |artifact, ordinal| {
            let groups = offer
                .group_bindings(NativeGroupKey {
                    artifact,
                    original_ordinal: ordinal,
                })
                .collect::<Vec<_>>();
            assert_eq!(groups.len(), 1);
            groups[0]
        };
        let left = binding(parent.descriptor.id, 8);
        let right = binding(parent.descriptor.id, 11);
        let child = binding(child.descriptor.id, 13);
        let NativeOfferSourceReceipt::Solved(receipt) = &offer.0.custody.source_receipt else {
            panic!("actual issuer missing")
        };
        assert_eq!(
            receipt.entries.len(),
            4,
            "two own roots and two exact edges, without a sibling root"
        );
        assert!(matches!(
            offer.source_causes_at(left, child).unwrap().as_ref(),
            [NativeSourceCause::Current]
        ));
        assert!(
            offer.source_causes_at(left, right).is_err(),
            "unselected parent is outside the selected path"
        );
        assert!(matches!(
            offer.source_causes_at(right, child).unwrap().as_ref(),
            [NativeSourceCause::Current]
        ));
        assert!(offer.source_causes_at(child, left).is_err());
        let graph = ArtifactView::from_custody(Arc::clone(offer.custody_lease()))
            .select_issued(vec![offer.original()], &offer.proof().unwrap().selection)
            .unwrap();
        let graph_only =
            IssuedNativeOffer::restore_snapshot(&offer.snapshot_facts(), &graph).unwrap();
        assert!(
            matches!(
                graph_only.source_causes_at(left, child).unwrap().as_ref(),
                [NativeSourceCause::Unwitnessed]
            ),
            "authenticated historical graph cannot acquire the live producer's source cause"
        );

        // This representation control observes causes; it does not construct an offer.
        let acquired =
            compiler_projection::SelectedNativeSupport::Acquired(Arc::from([offer.clone()]));
        let mut causes = NativeSourceReceipts::default();
        let key = NativeSourceUse::Root(child);
        causes.insert(
            key,
            NativeSourceCause::Historical {
                selected_at: left,
                support: acquired.clone(),
            },
        );
        causes.insert(
            key,
            NativeSourceCause::Historical {
                selected_at: right,
                support: acquired,
            },
        );
        assert_eq!(
            causes.0[&key].len(),
            2,
            "same offer under distinct selected anchors cannot collapse"
        );
        causes.insert(
            key,
            NativeSourceCause::Historical {
                selected_at: left,
                support: compiler_projection::SelectedNativeSupport::missing(),
            },
        );
        assert!(
            matches!(&causes.0[&key][0], NativeSourceCause::Historical { selected_at, support: compiler_projection::SelectedNativeSupport::Missing(_) } if *selected_at == left)
        );
        assert!(
            matches!(&causes.0[&key][1], NativeSourceCause::Historical { selected_at, support: compiler_projection::SelectedNativeSupport::Acquired(_) } if *selected_at == right)
        );
        causes.insert(key, NativeSourceCause::Unwitnessed);
        assert_eq!(
            causes.0[&key].len(),
            3,
            "absent issuer remains distinct from selected missing support"
        );
    }

    #[test]
    fn contribution_restriction_shares_only_unchanged_closed_membership() {
        let producer = CanonicalProducerIdentity::from_producer_bytes(b"contribution-sharing");
        let product = fixture_finalized_product(
            original_groups_fixture_with_interface(
                "ContributionSharing",
                vec![(7, vec![]), (9001, vec![])],
                29,
                &BTreeMap::new(),
                b"contribution interface".to_vec(),
            ),
            producer.sha256(),
        );
        let products = recovered_witness_fixtures(&[product])
            .into_iter()
            .map(|witness| witness.product)
            .collect::<Vec<_>>();
        let entries = products
            .iter()
            .map(|product| {
                Arc::new(ArtifactEntry::original(producer.sha256(), product.clone()).unwrap())
            })
            .collect::<Vec<_>>();
        let inventory = ArtifactInventory::default();
        let artifacts = inventory
            .admit_shared_with_demand(
                &inventory.empty_view(),
                entries.clone(),
                NativeArtifactDemand::ScopeInterfaces,
            )
            .unwrap();
        let projection = CompilerInputProjection::from_issued_entries(&entries).unwrap();
        let mut validation = crate::recovery_artifacts::PackageInterfaceValidation::default();
        let mut selection =
            crate::certified_products::CertifiedSourceSelection::from_compiler_projection(
                &projection,
                &artifacts.metadata_snapshot(),
                &validation.inventory,
            )
            .unwrap();
        let stage = crate::declaration_context::CapturedCompilerStage::issue(
            &mut selection,
            &artifacts,
            Vec::new(),
            &mut validation,
        )
        .unwrap();
        let inputs = stage.compiler_inputs();
        let original = ArtifactEntry::original(producer.sha256(), products[0].clone())
            .unwrap()
            .descriptor
            .id;
        let offer = inputs
            .issued_offers()
            .iter()
            .find(|offer| offer.original() == original)
            .unwrap();
        assert!(offer.is_full());
        let issued = Arc::new(
            offer
                .contribution_for(&BTreeSet::from([original]))
                .unwrap()
                .unwrap(),
        );
        let key = |original_ordinal| NativeGroupKey {
            artifact: original,
            original_ordinal,
        };
        let roots = [7, 9001]
            .into_iter()
            .flat_map(|ordinal| issued.groups(key(ordinal)))
            .collect::<BTreeSet<_>>();
        assert_eq!(roots.len(), 2);
        let closed = issued.for_groups(&roots).unwrap();
        assert_eq!(closed.reachable, issued.reachable);
        assert!(Arc::ptr_eq(&issued, &closed));
        let identity = closed.identity().unwrap();
        let repeated = closed.for_groups(&roots).unwrap();
        assert!(Arc::ptr_eq(&closed, &repeated));
        assert_eq!(repeated.identity.get(), Some(&identity));
        assert_eq!(
            repeated.snapshot_facts().unwrap(),
            closed.snapshot_facts().unwrap()
        );
        assert!(repeated.offer().same_issuance(offer));

        let only_seven = closed.groups(key(7)).collect::<BTreeSet<_>>();
        assert_eq!(only_seven.len(), 1);
        let narrowed = closed.for_groups(&only_seven).unwrap();
        assert!(!Arc::ptr_eq(&closed, &narrowed));
        assert!(narrowed.identity.get().is_none());
        assert!(narrowed.permits(key(7)));
        assert!(!narrowed.permits(key(9001)));
        assert!(closed.permits(key(9001)));
        assert!(narrowed.offer().same_issuance(offer));
        let narrow_identity = narrowed.identity().unwrap();
        let narrow_repeat = narrowed.for_groups(&only_seven).unwrap();
        assert!(Arc::ptr_eq(&narrowed, &narrow_repeat));
        assert_eq!(narrow_repeat.identity.get(), Some(&narrow_identity));

        let empty = narrowed.for_groups(&BTreeSet::new()).unwrap();
        assert!(!Arc::ptr_eq(&narrowed, &empty));
        assert!(empty.originals().is_empty());
        assert!(!empty.permits(key(7)));
        assert!(!empty.permits(key(9001)));
        assert!(empty.offer().same_issuance(offer));
        assert!(Arc::ptr_eq(
            &empty,
            &empty.for_groups(&BTreeSet::new()).unwrap()
        ));

        // All Group roots do not cover an independently retained canonical root.
        let sibling = fixture_finalized_product(
            original_groups_fixture_with_interface(
                "ContributionArtifactOnly",
                vec![(13, vec![])],
                30,
                &BTreeMap::new(),
                b"artifact-only interface".to_vec(),
            ),
            producer.sha256(),
        );
        let sibling = ArtifactEntry::canonical(sibling.module_interface().clone());
        let sibling_id = sibling.descriptor.id;
        let custody = ArtifactView::from_custody(Arc::clone(offer.custody_lease()));
        let issuer = custody
            .select_issued(vec![original], &offer.proof().unwrap().selection)
            .unwrap();
        let broad = issuer
            .inventory()
            .admit_shared(&issuer, vec![Arc::new(sibling)])
            .unwrap();
        let proof = NativeOriginalRequirement {
            original,
            selection: ExactArtifactSelection::capture(&broad),
        };
        let broad_offer = stage
            .compiler_inputs()
            .compiler_source_support()
            .retain_custody(&broad)
            .unwrap()
            .issue_native_offer(&entries[0].descriptor.owner, Some(proof))
            .unwrap();
        assert!(broad_offer.is_full());
        let mixed = Arc::new(
            broad_offer
                .contribution_for(&BTreeSet::from([original]))
                .unwrap()
                .unwrap(),
        );
        assert!(mixed.reachable.iter().any(|node| matches!(node,
            ArtifactBindingNode::Artifact(binding) if binding.artifact == sibling_id)));
        let all_groups = mixed
            .reachable
            .iter()
            .filter_map(|node| match node {
                ArtifactBindingNode::Group(group) => Some(*group),
                _ => None,
            })
            .collect::<BTreeSet<_>>();
        assert_eq!(all_groups, roots);
        let mixed_identity = mixed.identity().unwrap();
        let reduced = mixed.for_groups(&all_groups).unwrap();
        assert!(!Arc::ptr_eq(&mixed, &reduced));
        assert!(reduced.identity.get().is_none());
        assert_eq!(reduced.reachable, closed.reachable);
        assert!(!reduced
            .reachable
            .iter()
            .any(|node| node.artifact() == sibling_id));
        assert!(reduced.offer().same_issuance(&broad_offer));
        assert_ne!(reduced.identity().unwrap(), mixed_identity);
        assert_ne!(
            reduced.identity().unwrap(),
            closed.identity().unwrap(),
            "equal memberships from distinct offer proofs retain their own identity"
        );
    }

    fn exhaustive_group_supports<'a>(
        rows: impl Iterator<Item = &'a Arc<crate::declaration_context::NativeTransportAvailability>>,
        key: NativeGroupKey,
        owner: &tidepool_repr::execution_schema::CachedHomeOwner,
    ) -> Vec<(NativeGroupBinding, &'a IssuedNativeOffer)> {
        let mut expected = BTreeMap::new();
        for row in rows {
            if let Some(support) = row.dependency_support(key, owner) {
                for group in support.groups() {
                    if let Some(offer) = support.offer_for_group(group) {
                        expected.entry(group).or_insert(offer);
                    }
                }
            }
        }
        expected.into_iter().collect()
    }

    fn assert_same_supports(
        actual: Vec<(NativeGroupBinding, &IssuedNativeOffer)>,
        expected: Vec<(NativeGroupBinding, &IssuedNativeOffer)>,
    ) {
        assert_eq!(actual.len(), expected.len());
        for ((actual_group, actual_offer), (expected_group, expected_offer)) in
            actual.into_iter().zip(expected)
        {
            assert_eq!(actual_group, expected_group);
            assert!(actual_offer.same_issuance(expected_offer));
        }
    }

    #[test]
    fn normalized_source_keeps_retained_root_dependencies_without_dependency_roles_or_siblings() {
        let producer_bytes = b"normalized-source-dependency-closure";
        let producer = CanonicalProducerIdentity::from_producer_bytes(producer_bytes);
        let hidden = fixture_finalized_product(
            original_groups_fixture_with_interface(
                "NormalizedHidden",
                vec![(7, vec![]), (9, vec![])],
                73,
                &BTreeMap::new(),
                b"hidden interface".to_vec(),
            ),
            producer.sha256(),
        );
        let root = fixture_finalized_product(
            original_groups_fixture_with_interface(
                "NormalizedRoot",
                vec![(
                    3,
                    vec![PendingImportOwner::Source {
                        owner: hidden.owner().clone(),
                        original_ordinal: 7,
                        binder: tidepool_repr::execution_schema::testing::identity(
                            "NormalizedHidden",
                            "entry_7",
                        ),
                    }],
                )],
                74,
                &BTreeMap::new(),
                b"root interface".to_vec(),
            ),
            producer.sha256(),
        );
        let sibling = fixture_finalized_product(
            original_groups_fixture_with_interface(
                "NormalizedSibling",
                vec![(11, vec![])],
                75,
                &BTreeMap::new(),
                b"sibling interface".to_vec(),
            ),
            producer.sha256(),
        );
        let products = recovered_witness_fixtures(&[hidden, root, sibling])
            .into_iter()
            .map(|witness| witness.product)
            .collect::<Vec<_>>();
        let entries = products
            .iter()
            .map(|product| {
                Arc::new(ArtifactEntry::original(producer.sha256(), product.clone()).unwrap())
            })
            .collect::<Vec<_>>();
        let inventory = ArtifactInventory::default();
        let artifacts = inventory
            .admit_shared_with_demand(
                &inventory.empty_view(),
                entries.clone(),
                NativeArtifactDemand::ScopeInterfaces,
            )
            .unwrap();
        let projection = CompilerInputProjection::from_issued_entries(&entries).unwrap();
        let mut selection =
            crate::certified_products::CertifiedSourceSelection::from_compiler_projection(
                &projection,
                &artifacts.metadata_snapshot(),
                &tidepool_repr::execution_schema::InventoryOperation::new(Default::default()),
            )
            .unwrap();
        let inputs = crate::declaration_context::CapturedCompilerStage::issue(
            &mut selection,
            &artifacts,
            entries.clone(),
            &mut crate::recovery_artifacts::PackageInterfaceValidation::default(),
        )
        .unwrap();
        let complete =
            CompilerSourceSupport::close_complete_output(inputs.compiler_inputs()).unwrap();
        let root_owner = ExactModuleIdentity {
            unit: products[1].owner().unit.clone(),
            module: products[1].owner().module.clone(),
        };
        let interfaces = complete.selected_interfaces().unwrap();
        let types = interfaces
            .support_for_owners(vec![root_owner.clone()])
            .unwrap();
        let retained = types.retain_template_availability(&complete).unwrap();
        assert_eq!(retained.compiler_roles().len(), 1);
        assert!(retained
            .compiler_roles()
            .iter()
            .all(|(_, role)| role.original().is_none()));
        let hidden_id = ArtifactEntry::original(producer.sha256(), products[0].clone())
            .unwrap()
            .descriptor
            .id;
        let sibling_id = ArtifactEntry::original(producer.sha256(), products[2].clone())
            .unwrap()
            .descriptor
            .id;
        // Duplicate inputs pass through the real row issuer and collection
        // merge; no test-only row or contribution authority is constructed.
        let repeated_offers = complete
            .native_offers()
            .iter()
            .chain(complete.native_offers())
            .cloned()
            .collect::<Vec<_>>();
        let metadata = ExactDeclarationContext::new(&[], &[], vec![])
            .unwrap()
            .extend_issued_compiler_support(&complete)
            .unwrap()
            .compiler_metadata_snapshot()
            .unwrap();
        let repeated = crate::declaration_context::RequestNativeAvailability::issue_checked(
            complete.compiler_roles(),
            &metadata,
            complete.artifacts(),
            &repeated_offers,
        )
        .unwrap();
        let repeated = complete.retain_normalized_availability(&repeated).unwrap();
        let empty = crate::declaration_context::RequestNativeAvailability::issue_checked(
            complete.compiler_roles(),
            &metadata,
            complete.artifacts(),
            &[],
        )
        .unwrap();
        assert_eq!(empty.rows().count(), products.len());
        assert!(
            complete
                .native_availability()
                .rows()
                .any(|row| row.offers().len() > 1),
            "the fixture includes overlapping native contributions"
        );
        let originals = products
            .iter()
            .map(|product| {
                ArtifactEntry::original(producer.sha256(), product.clone())
                    .unwrap()
                    .descriptor
                    .id
            })
            .chain([ArtifactId([255; 32])])
            .collect::<Vec<_>>();
        for source in [&complete, &retained, &repeated] {
            let catalog = NativeDependencyCatalog::from_source(source).unwrap();
            for original in &originals {
                for original_ordinal in [3, 7, 9, 11, u32::MAX] {
                    let key = NativeGroupKey {
                        artifact: *original,
                        original_ordinal,
                    };
                    // Cross every exact owner, including mismatches for an
                    // existing payload and a wholly absent payload key.
                    for product in &products {
                        assert_same_supports(
                            catalog.group_supports(key, product.owner()),
                            exhaustive_group_supports(
                                source.native_availability().rows(),
                                key,
                                product.owner(),
                            ),
                        );
                        assert!(empty.dependency_support(key, product.owner()).is_none());
                        assert!(
                            exhaustive_group_supports(empty.rows(), key, product.owner())
                                .is_empty()
                        );
                    }
                }
            }
        }
        let catalog = NativeDependencyCatalog::from_source(&retained).unwrap();
        assert!(!catalog
            .group_supports(
                NativeGroupKey {
                    artifact: hidden_id,
                    original_ordinal: 7
                },
                products[0].owner()
            )
            .is_empty());
        assert!(catalog
            .group_supports(
                NativeGroupKey {
                    artifact: hidden_id,
                    original_ordinal: 9
                },
                products[0].owner()
            )
            .is_empty());
        assert!(catalog
            .group_supports(
                NativeGroupKey {
                    artifact: sibling_id,
                    original_ordinal: 11
                },
                products[2].owner()
            )
            .is_empty());
        let context = ExactDeclarationContext::new(&[], &[], vec![])
            .unwrap()
            .extend_issued_compiler_support(&retained)
            .unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let request = ExactCompileContext::new(Arc::new(context))
            .prepare_compilation(scratch.path(), producer_bytes)
            .unwrap();
        let hidden = request
            .compiler_inputs()
            .native_availability(hidden_id)
            .expect("retained R carries dependency H without an H role");
        assert!(hidden.permits(7));
        assert!(!hidden.permits(9));
        assert!(request
            .compiler_inputs()
            .native_availability(sibling_id)
            .is_none());
        assert!(request.groups.is_empty());

        let parent = complete.artifacts();
        let inventory = parent.inventory();
        let entries = parent
            .entries()
            .into_iter()
            .map(|entry| (entry.descriptor.id, entry))
            .collect::<BTreeMap<_, _>>();
        let root_id = ArtifactEntry::original(producer.sha256(), products[1].clone())
            .unwrap()
            .descriptor
            .id;
        let root_key = NativeGroupKey {
            artifact: root_id,
            original_ordinal: 3,
        };
        let proof = complete
            .native_offers()
            .iter()
            .find(|offer| offer.original() == root_id)
            .unwrap()
            .proof()
            .unwrap()
            .clone();
        // Independent oracle: literal rows recorded by the physical issuer,
        // captured before the receiving resolver runs.
        let graph = proof.selection.graph_selection();
        let expected_root = *graph
            .native_groups
            .iter()
            .find(|group| group.binding.artifact() == root_id && group.original_ordinal == 3)
            .unwrap();
        let rows = graph
            .selections
            .iter()
            .flat_map(|scope| scope.dependencies.iter())
            .cloned()
            .collect::<BTreeSet<_>>();
        let expected_hidden = rows
            .iter()
            .find_map(|(from, target, edge)| match target {
                InventoryNodeKey::Group(child)
                    if *from == InventoryNodeKey::Group(expected_root)
                        && *edge
                            == ArtifactDependency::NativeGroup {
                                dependent_ordinal: 3,
                                required_ordinal: 7,
                            } =>
                {
                    Some(*child)
                }
                _ => None,
            })
            .unwrap();
        assert_eq!(expected_hidden.binding.artifact(), hidden_id);
        let raw_pair = |group: NativeGroupBinding| {
            let carrier = rows
                .iter()
                .find_map(|(from, target, edge)| match target {
                    InventoryNodeKey::Artifact(binding)
                        if *from == InventoryNodeKey::Group(group)
                            && binding.artifact == group.binding.artifact()
                            && *edge == ArtifactDependency::Interface =>
                    {
                        Some(*binding)
                    }
                    _ => None,
                })
                .unwrap();
            let canonical = rows
                .iter()
                .find_map(|(from, target, edge)| match target {
                    InventoryNodeKey::Artifact(binding)
                        if *from == InventoryNodeKey::Artifact(carrier)
                            && *edge == ArtifactDependency::Interface =>
                    {
                        Some(*binding)
                    }
                    _ => None,
                })
                .unwrap();
            (carrier, canonical)
        };
        let root_pair = raw_pair(expected_root);
        let hidden_pair = raw_pair(expected_hidden);
        let root_originals = selection
            .selected_authored_original_closure(
                inputs.issued_artifacts(),
                &products[1].owner().unit,
                &products[1].owner().module,
            )
            .unwrap();
        let root_source = complete
            .certify_original_closure(&root_originals)
            .unwrap()
            .source_support(&BTreeSet::from([root_owner.clone()]))
            .unwrap();
        assert_eq!(root_source.compiler_roles().len(), 1);
        assert!(root_source.compiler_roles().get(&root_owner).is_some());
        let root_context = ExactDeclarationContext::new(&[], &[], vec![])
            .unwrap()
            .extend_issued_compiler_support(&root_source)
            .unwrap();
        let root_request = root_context
            .capture_world_primary_inputs(producer.sha256())
            .unwrap()
            .inputs()
            .clone();
        assert!(root_request
            .artifacts()
            .capture_graph_selection()
            .native_groups
            .contains(&expected_root));
        let selection = crate::certified_products::CertifiedSourceSelection::from_compiler_inputs(
            &root_request,
            &tidepool_repr::execution_schema::InventoryOperation::new(Default::default()),
        )
        .unwrap();
        let canonical = Arc::new(ArtifactEntry::canonical(
            products[0].module_interface().clone(),
        ));
        let canonical_id = canonical.descriptor.id;
        let receiving = inventory
            .admit_shared(&inventory.empty_view(), vec![canonical.clone()])
            .unwrap()
            .retain_custody(parent)
            .unwrap();
        let original_overlay = inventory
            .admit_shared_with_demand(
                &inventory.empty_view(),
                vec![entries[&hidden_id].clone()],
                NativeArtifactDemand::ScopeInterfaces,
            )
            .unwrap()
            .retain_custody(parent)
            .unwrap();
        assert_ne!(receiving.namespace()[&canonical_id], hidden_pair.1);
        assert!(
            NativeOriginalRequirement::from_issued_group_custody(
                hidden_id,
                canonical_id,
                &receiving
            )
            .unwrap()
            .is_none(),
            "new Full qualification cannot borrow another canonical scope"
        );
        let unrelated = Arc::new(ArtifactEntry::canonical(
            crate::certified_products::fixture_module_interface(
                producer.sha256(),
                "fixture",
                "CarrierNamespaceOverlay",
                BTreeMap::new(),
            ),
        ));
        let incompatible = inventory
            .admit_shared_with_demand(
                &inventory.empty_view(),
                entries.values().cloned().chain([unrelated]).collect(),
                NativeArtifactDemand::ScopeInterfaces,
            )
            .unwrap()
            .retain_custody(parent)
            .unwrap();
        let incompatible_namespace = incompatible.namespace();
        assert_ne!(incompatible_namespace[&root_id], root_pair.0);
        let before_incompatible = incompatible.capture_graph_selection();
        let mut state = inventory.0.lock().expect("inventory lock");
        let error = bind_certified_input_demand(
            &mut state,
            &incompatible,
            &entries,
            BTreeMap::new(),
            &BTreeSet::from([root_key]),
            &selection,
        )
        .err()
        .expect("an issued choice cannot substitute the receiver's selected Original");
        assert!(matches!(error, CompileError::ArtifactInventory(ref error)
        if error.native_refusal == Some(NativeAdmissionRefusal::InputNoCompatibleCandidate {
            selected_original: incompatible_namespace.get(&root_id).copied(),
            canonical: root_pair.1.artifact,
            selected_canonical: incompatible_namespace.get(&root_pair.1.artifact).copied(),
        })));
        drop(state);
        assert_eq!(incompatible.capture_graph_selection(), before_incompatible);
        let cold_inventory = ArtifactInventory::default();
        let cold = cold_inventory
            .recover_selection(
                &cold_inventory.empty_view(),
                parent.entries(),
                &parent.capture_graph_selection(),
                &parent.binding_dependencies(),
            )
            .unwrap();
        for (stage, view) in [
            ("issuer", parent),
            ("canonical_overlay", &receiving),
            ("original_overlay", &original_overlay),
            ("cold_graph", &cold),
        ] {
            let own_inventory = view.inventory();
            let before = view.capture_graph_selection();
            let mut state = own_inventory.0.lock().expect("inventory lock");
            let acquired = bind_certified_input_demand(
                &mut state,
                view,
                &entries,
                BTreeMap::new(),
                &BTreeSet::from([root_key]),
                &selection,
            )
            .unwrap_or_else(|error| panic!("exact parent acquisition stage={stage}: {error}"));
            assert_eq!(acquired.bindings[&root_key].binding(), Some(expected_root));
            assert_eq!(acquired.bindings[&root_key].carrier_pair(), Some(root_pair));
            let issued = &acquired.bindings[&root_key];
            assert!(issued
                .closure()
                .contains(&InventoryNodeKey::Group(expected_hidden)));
            assert!(!issued.closure().iter().any(|node| matches!(node, InventoryNodeKey::Group(group) if (group.binding.artifact() == hidden_id && group.original_ordinal == 9) || group.binding.artifact() == sibling_id)));
            assert!(state.outgoing(InventoryNodeKey::Group(expected_root)).any(
                |(target, edge)| target == InventoryNodeKey::Group(expected_hidden)
                    && *edge
                        == ArtifactDependency::NativeGroup {
                            dependent_ordinal: 3,
                            required_ordinal: 7
                        }
            ));
            assert!(state
                .outgoing(InventoryNodeKey::Group(expected_hidden))
                .any(
                    |(target, edge)| target == InventoryNodeKey::Artifact(hidden_pair.0)
                        && *edge == ArtifactDependency::Interface
                ));
            assert!(state
                .outgoing(InventoryNodeKey::Artifact(hidden_pair.0))
                .any(
                    |(target, edge)| target == InventoryNodeKey::Artifact(hidden_pair.1)
                        && *edge == ArtifactDependency::Interface
                ));
            let before_reference = state.graph_visits.load(Ordering::Relaxed);
            let reference =
                admitted_closure(&state, [InventoryNodeKey::Group(expected_root)].into_iter())
                    .iter()
                    .copied()
                    .collect::<BTreeSet<_>>();
            assert_eq!(issued.closure(), &reference);
            assert!(state.graph_visits.load(Ordering::Relaxed) > before_reference);
            let before_consume = state.graph_visits.load(Ordering::Relaxed);
            let namespace = BTreeMap::new();
            for _ in 0..4 {
                acquired
                    .validate_dependency_namespace(&entries, &namespace)
                    .unwrap();
            }
            assert_eq!(state.graph_visits.load(Ordering::Relaxed), before_consume);
            drop(state);
            assert_eq!(
                view.capture_graph_selection(),
                before,
                "historical custody does not replace current roles or roots"
            );
        }
        // A genuinely changed selected Original remains incompatible with the
        // old dependency at the existing current-request namespace boundary.
        let changed = fixture_finalized_product(
            original_groups_fixture_with_interface(
                "NormalizedHidden",
                vec![(7, vec![])],
                99,
                &BTreeMap::new(),
                b"changed interface".to_vec(),
            ),
            producer.sha256(),
        );
        let changed = recovered_witness_fixtures(&[changed]).remove(0).product;
        let changed = Arc::new(ArtifactEntry::original(producer.sha256(), changed).unwrap());
        assert!(validate_consumed_native_namespace(
            &entries[&hidden_id],
            &BTreeMap::from([(changed.descriptor.owner.clone(), changed)])
        )
        .is_err());
        let before_receiving = receiving.capture_graph_selection();
        // Carry the same historical offer through an actual compiler request.
        // Its private body permission cannot substitute for a direct root choice;
        // the parent-edge acquisition above remains independently accepted.
        let hidden_owner = canonical.descriptor.owner.clone();
        let visible = interfaces
            .support_for_owners(vec![hidden_owner.clone()])
            .unwrap()
            .retain_template_availability(&retained)
            .unwrap();
        let context = ExactDeclarationContext::new(&[], &[], vec![])
            .unwrap()
            .extend_issued_compiler_support(&visible)
            .unwrap();
        let certified_request = ExactCompileContext::new(Arc::new(context))
            .prepare_compilation(&scratch.path().join("historical-consumer"), producer_bytes)
            .unwrap();
        let selection = crate::certified_products::CertifiedSourceSelection::from_compiler_inputs(
            certified_request.compiler_inputs(),
            &crate::recovery_artifacts::PackageInterfaceValidation::default().inventory,
        )
        .unwrap();
        assert_eq!(
            selection.interface_only_artifact(&hidden_owner),
            Some(canonical_id)
        );
        let mut validation = crate::recovery_artifacts::PackageInterfaceValidation::default();
        let fixed = inventory
            .bind_segment(&receiving, vec![], &selection, &mut validation)
            .unwrap();
        let cohort = fixed.captured_stage().physical_cohort();
        let physical = cohort.artifacts();
        assert!(!physical.namespace().contains_key(&hidden_id));
        assert_ne!(
            physical.namespace()[&canonical_id],
            parent.namespace()[&canonical_id]
        );
        let before_physical = physical.capture_graph_selection();
        let (prepared, accepted) =
            super::super::tests::source_target_data(&[(&entries[&hidden_id], 7)]);
        let observed = crate::certified_products::AuthenticatedTargetImports::issue(
            &prepared,
            &accepted,
            &validation.inventory,
        )
        .unwrap();
        let error = inventory
            .select_native_target(
                cohort,
                observed,
                None,
                Some(&inventory.empty_view()),
                &[],
                &BTreeMap::new(),
                &mut validation,
            )
            .err()
            .expect("a direct private Input demand lacks a root choice or current product");
        assert!(
            matches!(&error, CompileError::ArtifactInventory(error)
            if error.native_refusal == Some(NativeAdmissionRefusal::RootChoiceMissing {
                owner: hidden_owner,
            })),
            "a sole private issued candidate cannot bypass root choice: {error:?}"
        );
        assert_eq!(physical.capture_graph_selection(), before_physical);
        assert_eq!(receiving.capture_graph_selection(), before_receiving);
    }

    fn pending_selection(
        producer_bytes: &[u8],
        module: &str,
        ordinal: u32,
        seed: u8,
    ) -> (
        Arc<ArtifactEntry>,
        crate::certified_products::CertifiedSourceSelection,
    ) {
        let producer = CanonicalProducerIdentity::from_producer_bytes(producer_bytes);
        let product = fixture_finalized_product(
            original_groups_fixture_with_interface(
                module,
                vec![(ordinal, vec![])],
                seed,
                &BTreeMap::new(),
                format!("{module} interface").into_bytes(),
            ),
            producer.sha256(),
        );
        let entry = Arc::new(ArtifactEntry::original(producer.sha256(), product).unwrap());
        let inventory = ArtifactInventory::default();
        let artifacts = inventory
            .admit_recovery_selection(
                &inventory.empty_view(),
                vec![entry.clone()],
                &BTreeSet::from([NativeGroupKey {
                    artifact: entry.descriptor.id,
                    original_ordinal: ordinal,
                }]),
            )
            .unwrap();
        let projection = CompilerInputProjection::from_issued_entries(&[entry.clone()]).unwrap();
        let mut selection =
            crate::certified_products::CertifiedSourceSelection::from_compiler_projection(
                &projection,
                &artifacts.metadata_snapshot(),
                &tidepool_repr::execution_schema::InventoryOperation::new(Default::default()),
            )
            .unwrap();
        // Finish the product through the real completion issuer so the
        // selection carries issued availability rather than an unresolved
        // pending requirement.
        let completed = crate::declaration_context::CapturedCompilerStage::issue(
            &mut selection,
            &artifacts,
            vec![entry.clone()],
            &mut crate::recovery_artifacts::PackageInterfaceValidation::default(),
        )
        .unwrap();
        assert!(completed
            .compiler_inputs()
            .native_availability()
            .issued_row(entry.descriptor.id)
            .is_some());
        (entry, selection)
    }

    fn native_refusal_reason(error: &CompileError) -> String {
        let diagnostic = crate::failclass::classify_compile(error);
        let encoded = serde_json::to_value(diagnostic.cause).unwrap();
        encoded["native_refusal"]["reason"]
            .as_str()
            .unwrap_or_default()
            .to_owned()
    }

    /// A pending projection keeps the product's own transaction-held
    /// requirement and issues no completion row. The published-source role is
    /// what makes that requirement name the selected group.
    fn pending_projection_selection(
        producer_bytes: &[u8],
        module: &str,
        ordinal: u32,
        seed: u8,
    ) -> (
        Arc<ArtifactEntry>,
        crate::certified_products::CertifiedSourceSelection,
        NativeGroupBinding,
    ) {
        let producer = CanonicalProducerIdentity::from_producer_bytes(producer_bytes);
        let product = fixture_finalized_product(
            original_groups_fixture_with_interface(
                module,
                vec![(ordinal, vec![])],
                seed,
                &BTreeMap::new(),
                format!("{module} interface").into_bytes(),
            ),
            producer.sha256(),
        );
        let entry = Arc::new(ArtifactEntry::original(producer.sha256(), product).unwrap());
        let inventory = ArtifactInventory::default();
        let artifacts = inventory
            .admit_recovery_selection(
                &inventory.empty_view(),
                vec![entry.clone()],
                &BTreeSet::from([NativeGroupKey {
                    artifact: entry.descriptor.id,
                    original_ordinal: ordinal,
                }]),
            )
            .unwrap();
        let issued = artifacts.capture_graph_selection().native_groups;
        assert_eq!(issued.len(), 1);
        let expected_binding = *issued.iter().next().unwrap();
        let ArtifactPayload::Original(original) = &entry.payload else {
            unreachable!();
        };
        let birth = crate::declaration_context::issue_fixture_compiler_support(
            CompilerInputProjection::from_issued_entries(&[entry.clone()]).unwrap(),
            artifacts.clone(),
        )
        .unwrap();
        let mut projection = CompilerInputProjection::default();
        projection
            .admit_role(
                entry.descriptor.owner.clone(),
                CompilerInputRole::PublishedSourceOriginal {
                    interface: ArtifactEntry::canonical_artifact_id(original.module_interface()),
                    original: entry.descriptor.id,
                    source_revision: "pending fixture revision".into(),
                    original_input_identity: "pending fixture input".into(),
                    selection: ExactArtifactSelection::capture(&artifacts),
                    selection_sha256: [0; 32],
                    native_authority: PublicationBirthFacts::capture(&birth).unwrap(),
                },
            )
            .unwrap();
        let selection =
            crate::certified_products::CertifiedSourceSelection::from_compiler_projection(
                &projection,
                &artifacts.metadata_snapshot(),
                &tidepool_repr::execution_schema::InventoryOperation::new(Default::default()),
            )
            .unwrap();
        (entry, selection, expected_binding)
    }

    #[test]
    fn absent_root_choice_refuses_a_sole_pending_candidate() {
        let (entry, selection) =
            pending_selection(b"absent-root-choice", "AbsentRootChoice", 5, 90);
        let key = NativeGroupKey {
            artifact: entry.descriptor.id,
            original_ordinal: 5,
        };
        let ArtifactPayload::Original(product) = &entry.payload else {
            unreachable!();
        };
        assert!(
            !selection
                .native_group_candidates(key, product.owner())
                .unwrap()
                .bindings()
                .is_empty(),
            "the refusal control requires an otherwise compatible candidate"
        );
        let error = verified_root_choice(&selection, key, product.owner(), &entry).unwrap_err();
        assert_eq!(native_refusal_reason(&error), "root_choice_missing");
    }

    #[test]
    fn bound_current_product_preserves_its_carried_own_binding() {
        let (entry, selection, expected_binding) =
            pending_projection_selection(b"current-own-binding", "CurrentOwnBinding", 5, 94);
        let key = NativeGroupKey {
            artifact: entry.descriptor.id,
            original_ordinal: 5,
        };
        let current = selection.current_product(key, &entry).unwrap().unwrap();
        let binding = verified_current_product(&current).unwrap().unwrap();
        assert_eq!(binding, expected_binding);
    }

    #[test]
    fn issued_completion_authorizes_the_exact_current_product_binding() {
        let (entry, selection) = pending_selection(b"issued-completion", "IssuedCompletion", 5, 97);
        let key = NativeGroupKey {
            artifact: entry.descriptor.id,
            original_ordinal: 5,
        };
        let current = selection.current_product(key, &entry).unwrap().unwrap();
        let binding = verified_current_product(&current)
            .expect("issued completion authorizes its own product binding")
            .expect("issued completion names an existing exact binding");
        assert_eq!(binding.binding.artifact(), entry.descriptor.id);
        assert_eq!(binding.original_ordinal, 5);
    }

    /// A1c shape pinned at the inventory boundary: a carried root group names a
    /// child owner that exists only as an inherited canonical interface. The
    /// current resolver requires an Original owner row, so target admission
    /// refuses instead of widening the owner index or substituting canonical
    /// bytes. This pins the gap for the later availability-fragment repair.
    #[test]
    fn carried_group_cannot_bind_owner_present_only_as_canonical_interface() {
        let producer = CanonicalProducerIdentity::from_producer_bytes(b"a1c-canonical-only-child");
        let hidden = fixture_finalized_product(
            original_groups_fixture_with_interface(
                "CanonicalOnlyHidden",
                vec![(7, vec![])],
                95,
                &BTreeMap::new(),
                b"hidden interface".to_vec(),
            ),
            producer.sha256(),
        );
        let root = fixture_finalized_product(
            original_groups_fixture_with_interface(
                "CarriedRoot",
                vec![(
                    4,
                    vec![PendingImportOwner::Source {
                        owner: hidden.owner().clone(),
                        original_ordinal: 7,
                        binder: tidepool_repr::execution_schema::testing::identity(
                            "CanonicalOnlyHidden",
                            "entry_7",
                        ),
                    }],
                )],
                96,
                &BTreeMap::new(),
                b"root interface".to_vec(),
            ),
            producer.sha256(),
        );
        let hidden_interface =
            Arc::new(ArtifactEntry::canonical(hidden.module_interface().clone()));
        let root_entry = Arc::new(ArtifactEntry::original(producer.sha256(), root).unwrap());
        let inventory = ArtifactInventory::default();
        let inherited = inventory
            .admit_shared(&inventory.empty_view(), vec![hidden_interface])
            .unwrap();
        let error = inventory
            .admit_shared(&inherited, vec![root_entry])
            .expect_err("canonical-only child cannot resolve the carried native edge");
        assert!(
            matches!(error, CompileError::ArtifactInventory(ref error)
            if matches!(&error.failure, ArtifactInventoryFailure::MissingDependency { required, .. }
                if required.module == "CanonicalOnlyHidden")),
            "the carried edge must refuse rather than widen the owner index: {error:?}"
        );
    }
}
