use super::*;

/// Exact dependency grants remain attached to their issuing owner. Source and
/// request rows share permissions independently of their supporting proofs.
pub(crate) struct NativeDependencyCatalog<'a>(DependencyGrantSource<'a>);

enum DependencyGrantSource<'a> {
    Source(&'a CompilerSourceSupport),
    Request(&'a crate::declaration_context::ContinuationNativeInputs<'a>),
}

impl<'a> NativeDependencyCatalog<'a> {
    pub(crate) fn from_request(
        inputs: &'a crate::declaration_context::ContinuationNativeInputs<'a>,
    ) -> Self {
        Self(DependencyGrantSource::Request(inputs))
    }

    pub(crate) fn from_source(source: &'a CompilerSourceSupport) -> Result<Self, CompileError> {
        Ok(Self(DependencyGrantSource::Source(source)))
    }

    fn current_product<'b>(
        &'b self,
        key: NativeGroupKey,
        entry: &'b Arc<ArtifactEntry>,
    ) -> Result<Option<crate::certified_products::CurrentProductRef<'b>>, CompileError> {
        match &self.0 {
            DependencyGrantSource::Request(inputs) => inputs.current_product(key, entry),
            DependencyGrantSource::Source(_) => Ok(None),
        }
    }

    pub(crate) fn input_decisions(&self) -> &IssuedInputNativeDecisions {
        match &self.0 {
            DependencyGrantSource::Source(source) => source.input_decisions(),
            DependencyGrantSource::Request(inputs) => inputs.input_decisions(),
        }
    }

    pub(crate) fn decision<'b>(&'b self, owner: &ExactModuleIdentity) -> InputNativeDecision<'b> {
        let projection = match &self.0 {
            DependencyGrantSource::Source(source) => source.projection(),
            DependencyGrantSource::Request(inputs) => inputs.projection(),
        };
        match projection.snapshot_roles().get(owner) {
            Some(role) => self.input_decisions().for_role(owner, role),
            None => InputNativeDecision::UnissuedPolicy,
        }
    }

    /// Borrow historical qualifications only when this request's actual rows
    /// permit every exact binding. A retained Full offer alone grants nothing.
    pub(crate) fn qualified_availability(
        &self,
        projection: &CompilerInputProjection,
        namespace: &ArtifactView,
        advertised: &BTreeSet<NativeGroupKey>,
        new_products: &[IssuedNativeOffer],
    ) -> Result<Vec<QualifiedNativeAvailability>, CompileError> {
        let DependencyGrantSource::Request(inputs) = &self.0 else {
            return Err(failure("request qualifications require a request catalog"));
        };
        let mut qualifications = Vec::new();
        for row in inputs.availability_rows() {
            for offer in row.offers() {
                let Some(qualification) =
                    offer.qualified_private_availability(projection, namespace)
                else {
                    continue;
                };
                if !new_products
                    .iter()
                    .any(|product| product.original() == qualification.original())
                    || !advertised
                        .iter()
                        .any(|key| key.artifact == qualification.original())
                {
                    continue;
                }
                let mut permitted = true;
                for group in qualification.groups() {
                    let key = NativeGroupKey {
                        artifact: group.binding.artifact(),
                        original_ordinal: group.original_ordinal,
                    };
                    let Some(entry) = offer.0.custody.lease.payloads.get(&key.artifact) else {
                        return Err(failure("qualified native group lacks its issued payload"));
                    };
                    let ArtifactPayload::Original(product) = &entry.payload else {
                        return Err(failure("qualified native group lacks original payload"));
                    };
                    if !self.groups(key, product.owner())?.contains(group) {
                        permitted = false;
                        break;
                    }
                }
                if permitted && !qualifications.contains(qualification) {
                    qualifications.push(qualification.clone());
                }
            }
        }
        Ok(qualifications)
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
                if let Some(selected) = self.input_decisions().selected_support(*group) {
                    if let Some(offer) = selected
                        .acquisitions()
                        .iter()
                        .find(|offer| offer.contains_checked_group(*group))
                    {
                        supports.insert(*group, offer);
                    }
                }
            }
        }
        let source = match &self.0 {
            DependencyGrantSource::Source(source) => {
                source.native_availability().dependency_support(key, owner)
            }
            DependencyGrantSource::Request(_) => None,
        };
        let request = match &self.0 {
            DependencyGrantSource::Request(inputs) => Some(inputs.availability_rows()),
            DependencyGrantSource::Source(_) => None,
        };
        let request = request
            .into_iter()
            .flatten()
            .filter_map(|row| row.dependency_support(key, owner));
        for support in source.into_iter().chain(request) {
            for group in support.groups() {
                if let Some(offer) = support.offer_for_group(group) {
                    supports.entry(group).or_insert(offer);
                }
            }
        }
        supports.into_iter().collect()
    }

    fn groups(
        &self,
        key: NativeGroupKey,
        owner: &tidepool_repr::execution_schema::CachedHomeOwner,
    ) -> Result<imbl::OrdSet<NativeGroupBinding>, CompileError> {
        Ok(self
            .group_supports(key, owner)
            .into_iter()
            .map(|(group, _)| group)
            .collect())
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
    pub(super) fn key(&self) -> (ExactModuleIdentity, NativeGroupEdge) {
        (self.required_owner.clone(), self.edge)
    }
    pub(super) fn target(&self) -> &NativeGroupTarget {
        &self.target
    }
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
        Err(error @ CertificationError::InputDecision { reason, .. }) => {
            return Err(match root_evidence_refusal(&identity, reason) {
                Ok(refusal) => root_refusal(key, refusal),
                Err(_) => CompileError::CompilerEvidence(Box::new(error)),
            })
        }
        Err(error) => return Err(CompileError::CompilerEvidence(Box::new(error))),
    };
    let ArtifactPayload::Original(product) = &entry.payload else {
        return Err(failure("native root demand lacks original payload"));
    };
    let canonical = ArtifactEntry::canonical_artifact_id(
        product
            .module_interface()
            .ok_or_else(|| failure("native canonical carrier missing"))?,
    );
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
        Err(error @ CertificationError::InputDecision { reason, .. }) => {
            Err(match root_evidence_refusal(&identity, reason) {
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
        | InputDecisionRefusal::ArtifactIdentityMismatch
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
        if let Some(selected) = catalog.input_decisions().selected_support(binding) {
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
        let canonical = ArtifactEntry::canonical_artifact_id(
            product
                .module_interface()
                .ok_or_else(|| failure("native support lacks canonical interface"))?,
        );
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
                let selected = source.input_decisions().selected_support(chosen);
                let support = selected
                    .into_iter()
                    .flat_map(|selected| selected.acquisitions())
                    .find(|offer| offer.contains_checked_group(chosen))
                    .map(crate::certified_products::CertifiedNativeGroupSupport::Issued)
                    .or_else(|| selection.native_group_support(*key, owner, chosen))
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
        let canonical = ArtifactEntry::canonical_artifact_id(
            product
                .module_interface()
                .ok_or_else(|| failure("native canonical carrier missing"))?,
        );
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

/// Fresh bodies can depend on an available group without selecting it for
/// execution. Resolve only their certified imports from the supplied offers;
/// proof roots remain separate from these dependency choices.
pub(super) fn bind_fresh_product_dependencies(
    state: &mut InventoryState,
    parent: &ArtifactView,
    consumed_namespace: &BTreeMap<ExactModuleIdentity, Arc<ArtifactEntry>>,
    entries: &impl ArtifactEntryMap<ArtifactId>,
    fresh: &BTreeSet<NativeGroupKey>,
    catalog: &NativeDependencyCatalog<'_>,
    required: &mut BTreeMap<NativeGroupKey, imbl::OrdSet<NativeGroupBinding>>,
) -> Result<CertifiedNativeDemandAcquisition, CompileError> {
    let mut acquisition = CertifiedNativeDemandAcquisition {
        entries: BTreeMap::new(),
        bindings: BTreeMap::new(),
        offers: Vec::new(),
    };
    let mut pending = fresh.iter().copied().collect::<Vec<_>>();
    while let Some(group) = pending.pop() {
        if acquisition.bindings.contains_key(&group) {
            continue;
        }
        let entry = entries
            .get(&group.artifact)
            .ok_or_else(|| failure("fresh native product lacks physical payload"))?;
        let current = catalog.current_product(group, entry)?.ok_or_else(|| {
            native_admission_failure(
                group,
                NativeAdmissionRefusal::CurrentProductNotCompleted {
                    original: group.artifact,
                    original_ordinal: group.original_ordinal,
                },
            )
        })?;
        let resolved = if let Some(binding) = verified_current_product(&current)? {
            let support = current.own_group_support(binding).ok_or_else(|| {
                native_admission_failure(
                    group,
                    NativeAdmissionRefusal::InputIssuedSupportMissing { candidate: binding },
                )
            })?;
            let support = NativeSupportRef::checked(state, parent, binding, support)?;
            let issued = support.describe(binding)?;
            support.acquire(state, entries, &mut acquisition.entries, &issued)?;
            if let Some(offer) = support.offer {
                acquisition.offers.push(offer.clone());
            }
            VerifiedNativeBinding::CurrentIssued(issued)
        } else {
            let dependencies = current_product_dependencies(
                state,
                parent,
                entries,
                &mut acquisition.entries,
                &mut acquisition.offers,
                &current,
            )?;
            pending.extend(dependencies.iter().filter_map(
                |dependency| match dependency.target() {
                    NativeGroupTarget::NewGroup(group) => Some(*group),
                    NativeGroupTarget::ExactGroup(_) | NativeGroupTarget::CurrentIssued(_) => None,
                },
            ));
            VerifiedNativeBinding::NewCurrent { dependencies }
        };
        acquisition.bindings.insert(group, resolved);
    }
    acquisition.retain_dependency_groups(entries, consumed_namespace, required)?;
    Ok(acquisition)
}

impl CertifiedNativeDemandAcquisition {
    fn retain_dependency_groups(
        &self,
        entries: &impl ArtifactEntryMap<ArtifactId>,
        consumed_namespace: &BTreeMap<ExactModuleIdentity, Arc<ArtifactEntry>>,
        required: &mut BTreeMap<NativeGroupKey, imbl::OrdSet<NativeGroupBinding>>,
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
                required
                    .entry(NativeGroupKey {
                        artifact: group.binding.artifact(),
                        original_ordinal: group.original_ordinal,
                    })
                    .or_default()
                    .insert(*group);
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
        ArtifactPayload::Original(product) => product.module_interface(),
        ArtifactPayload::Interface(..) => None,
    };
    if canonical != product.module_interface()
        || (matches!(selected.payload, ArtifactPayload::Original(_))
            && selected.descriptor.id != dependency.descriptor.id)
    {
        tracing::warn!(
            target: "tidepool::artifact_inventory::namespace_conflict",
            owner = ?dependency.descriptor.owner,
            dependency = ?dependency.descriptor.id,
            selected = ?selected.descriptor.id,
            selected_kind = ?selected.descriptor.kind,
            interfaces_match = canonical == product.module_interface(),
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

impl PartialEq for IssuedNativeOffer {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0) || self.snapshot_facts() == other.snapshot_facts()
    }
}
impl Eq for IssuedNativeOffer {}

/// The exact portion of an admitted offer consumed by one request. Its
/// membership remains tied to that offer's proof and acquisition lifetimes.
#[derive(Clone, Debug)]
pub(crate) struct NativeOfferContribution {
    offer: IssuedNativeOffer,
    reachable: imbl::OrdSet<ArtifactBindingNode>,
    originals: BTreeSet<ArtifactId>,
    identity: OnceLock<[u8; 32]>,
}

impl PartialEq for NativeOfferContribution {
    fn eq(&self, other: &Self) -> bool {
        self.offer == other.offer
            && self.reachable == other.reachable
            && self.originals == other.originals
    }
}
impl Eq for NativeOfferContribution {}

#[cfg(test)]
thread_local! {
    static GROUP_BINDING_SCOPE_VISITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

impl NativeOfferContribution {
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
        Ok(self.restrict_groups(roots, None)?.0)
    }

    pub(crate) fn for_groups_with_operation(
        self: &Arc<Self>,
        roots: &BTreeSet<NativeGroupBinding>,
        operation: &tidepool_repr::execution_schema::InventoryOperation,
    ) -> Result<(Arc<Self>, NativeClosureWork), CompileError> {
        self.restrict_groups(roots, Some(operation))
    }

    fn restrict_groups(
        self: &Arc<Self>,
        roots: &BTreeSet<NativeGroupBinding>,
        operation: Option<&tidepool_repr::execution_schema::InventoryOperation>,
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
        let (reachable, work) = admitted_closure_with_operation(
            &state,
            roots
                .iter()
                .copied()
                .filter(|group| self.reachable.contains(&ArtifactBindingNode::Group(*group)))
                .map(ArtifactBindingNode::Group),
            operation,
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

    pub(crate) fn has_observed_original_owner(&self, owner: &ExactModuleIdentity) -> bool {
        self.offer()
            .custody_lease()
            .acquired_owner_counts
            .contains_key(owner)
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
    pub(super) fn from_complete_custody(
        admitted: IssuedArtifactCustody,
        originals: &[ArtifactId],
    ) -> Result<Self, CompileError> {
        let entries = admitted.entries.clone();
        Ok(Self {
            offers: IssuedNativeOffer::issue_complete_custody(admitted, originals)?,
            entries,
            roots: originals.iter().copied().collect(),
        })
    }

    pub(crate) fn validate_request(
        self,
        baseline_groups: &BTreeSet<NativeGroupKey>,
        catalog: &NativeDependencyCatalog<'_>,
        projection: &CompilerInputProjection,
        namespace: &ArtifactView,
        validation: &mut crate::recovery_artifacts::PackageInterfaceValidation,
    ) -> Result<ValidatedFreshNativeAvailability, CompileError> {
        let advertised = self.advertised_groups(baseline_groups)?;
        let preferred =
            catalog.qualified_availability(projection, namespace, &advertised, &self.offers)?;
        self.validate_advertised(advertised, &preferred, validation)
    }

    fn advertised_groups(
        &self,
        baseline: &BTreeSet<NativeGroupKey>,
    ) -> Result<BTreeSet<NativeGroupKey>, CompileError> {
        let mut advertised = baseline.clone();
        for offer in &self.offers {
            let entry = self
                .entries
                .get(&offer.original())
                .ok_or_else(|| failure("private original is outside retained custody"))?;
            let ArtifactPayload::Original(product) = &entry.payload else {
                return Err(failure("private original lacks original payload"));
            };
            advertised.extend(
                crate::certified_products::original_available_groups(product)
                    .map_err(|error| CompileError::CompilerEvidence(Box::new(error)))?
                    .map(|group| NativeGroupKey {
                        artifact: offer.original(),
                        original_ordinal: group.original_ordinal(),
                    }),
            );
        }
        Ok(advertised)
    }

    fn validate_advertised(
        self,
        advertised: BTreeSet<NativeGroupKey>,
        preferred: &[QualifiedNativeAvailability],
        validation: &mut crate::recovery_artifacts::PackageInterfaceValidation,
    ) -> Result<ValidatedFreshNativeAvailability, CompileError> {
        let originals = self
            .entries
            .iter()
            .filter_map(|(id, entry)| match &entry.payload {
                ArtifactPayload::Original(product) => Some((*id, product)),
                _ => None,
            })
            .collect::<BTreeMap<_, _>>();
        let mut qualifications = BTreeMap::new();
        for qualification in preferred {
            if qualifications
                .insert(qualification.original(), qualification.clone())
                .is_some_and(|prior| prior != *qualification)
            {
                return Err(admission_failure(
                    ArtifactInventoryFailure::BindingAmbiguity {
                        artifact: qualification.original(),
                    },
                ));
            }
        }
        for offer in &self.offers {
            if let Some(qualification) = offer.full_qualification() {
                if qualifications
                    .insert(qualification.original(), qualification.clone())
                    .is_some_and(|prior| prior != *qualification)
                {
                    return Err(admission_failure(
                        ArtifactInventoryFailure::BindingAmbiguity {
                            artifact: qualification.original(),
                        },
                    ));
                }
            }
        }
        crate::certified_products::validate_available_originals_with_validation(
            &originals,
            &advertised,
            &qualifications.into_values().collect::<Vec<_>>(),
            validation,
        )
        .map_err(|error| CompileError::CompilerEvidence(Box::new(error)))?;
        Ok(ValidatedFreshNativeAvailability {
            roots: self.roots,
            offers: self.offers,
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
    fn issue_complete_custody(
        admitted: IssuedArtifactCustody,
        originals: &[ArtifactId],
    ) -> Result<Vec<Self>, CompileError> {
        let source_receipt = admitted.source_receipt;
        let source = admitted.lease;
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
            let entry = source
                .payloads
                .get(&original)
                .ok_or_else(|| failure("complete availability lacks original custody"))?;
            let ArtifactPayload::Original(product) = &entry.payload else {
                return Err(admission_failure(
                    ArtifactInventoryFailure::NativeRootNotOriginal { artifact: original },
                ));
            };
            let interface = ArtifactEntry::canonical_artifact_id(
                product
                    .module_interface()
                    .ok_or_else(|| failure("complete availability lacks canonical interface"))?,
            );
            let carrier = *source
                .selection
                .namespace
                .get(&original)
                .ok_or_else(|| failure("complete availability lacks exact original carrier"))?;
            let canonical =
                *source.selection.namespace.get(&interface).ok_or_else(|| {
                    failure("complete availability lacks exact canonical carrier")
                })?;
            if ambiguous_roots.contains(&(original, carrier)) {
                return Err(admission_failure(
                    ArtifactInventoryFailure::BindingAmbiguity { artifact: original },
                ));
            }
            let roots = issued_roots
                .remove(&(original, carrier))
                .unwrap_or_default();
            if !entry
                .native_group_ordinals
                .iter()
                .all(|ordinal| roots.contains_key(ordinal))
            {
                return Err(failure(
                    "complete availability lacks its issued group census",
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
            let selected = selection_snapshot_for_lease(
                &state,
                &BTreeMap::from([(original, carrier), (interface, canonical)]),
                &selected_roots,
                &[],
                &[],
            );
            // This pass is the acquisition-time complete closure validation;
            // demand consumers retain the proof and query exact rows by key.
            let nodes = admitted_closure(&state, selected_roots.iter().copied());
            let groups = nodes
                .iter()
                .filter_map(|node| match node {
                    ArtifactBindingNode::Group(group) => Some(*group),
                    _ => None,
                })
                .collect();
            let mut custody_selection = (*selected).clone();
            custody_selection.custody_roots = custody_selection.roots.clone();
            custody_selection.roots.clear();
            custody_selection.namespace.clear();
            let mut payloads = Default::default();
            let mut acquired_owner_counts = Default::default();
            for id in artifact_ids(&nodes) {
                ViewLease::insert_payload(
                    &mut payloads,
                    &mut acquired_owner_counts,
                    id,
                    Arc::clone(&source.payloads[&id]),
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
                body,
                lease,
                nodes,
                roots,
            ));
        }
        drop(state);
        offers
            .into_iter()
            .map(|(original, interface, owner, body, lease, nodes, roots)| {
                Ok(Self(Arc::new(NativeOfferFacts {
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
            })
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

    /// The immutable declaration context is an issuance ingress. Capture its
    /// own roles and graph before merging any private input. Protected roles
    /// whose carrier is not present here wait for their exact private offer.
    pub(crate) fn issue_context_offers(
        projection: &CompilerInputProjection,
        source: &ArtifactView,
        preferred: &[NativeOriginalRequirement],
    ) -> Result<Vec<Self>, CompileError> {
        let mut offers = Vec::new();
        for role in projection.roles() {
            let Some(original) = role.original() else {
                continue;
            };
            if !source.lease.payloads.contains_key(&original) {
                continue;
            }
            let mut proofs = match NativeOriginalRequirement::from_role(role.clone()) {
                Some(proof) => vec![proof],
                None => preferred
                    .iter()
                    .filter(|proof| proof.original == original)
                    .cloned()
                    .collect(),
            };
            if proofs.is_empty() {
                proofs.extend(NativeOriginalRequirement::from_selected_view(
                    original, source,
                )?);
            }
            if proofs.is_empty() {
                let complete = match NativeOriginalRequirement::from_issued_group_custody(
                    original,
                    role.interface(),
                    source,
                ) {
                    Ok(proof) => proof,
                    // Optional baseline discovery has no issued group choice.
                    // Competing custody cannot issue Full or prevent a separate
                    // exact private offer from supplying its own authority.
                    Err(CompileError::ArtifactInventory(error)) if matches!(error.failure, ArtifactInventoryFailure::BindingAmbiguity { artifact } if artifact == original) => {
                        None
                    }
                    Err(error) => return Err(error),
                };
                proofs.extend(complete);
            }
            if proofs.is_empty() {
                offers.push(Self::issue(original, source, None)?);
            } else {
                for proof in proofs {
                    offers.push(Self::issue(original, source, Some(proof))?);
                }
            }
        }
        Ok(offers)
    }

    /// Issuance is restricted to an input owner's view before independent
    /// inputs merge. `None` issues empty availability rather than discovery.
    pub(crate) fn issue(
        original: ArtifactId,
        source: &ArtifactView,
        proof: Option<NativeOriginalRequirement>,
    ) -> Result<Self, CompileError> {
        let entry = source
            .lease
            .payloads
            .get(&original)
            .ok_or_else(|| failure("native offer original is outside issuer custody"))?;
        let ArtifactPayload::Original(product) = &entry.payload else {
            return Err(admission_failure(
                ArtifactInventoryFailure::NativeRootNotOriginal { artifact: original },
            ));
        };
        let interface = ArtifactEntry::canonical_artifact_id(
            product
                .module_interface()
                .ok_or_else(|| failure("native offer lacks its canonical interface"))?,
        );
        let (body, custody) = match proof {
            None => {
                let state = source.lease.inventory.0.lock().expect("inventory lock");
                let binding =
                    source.resolve_binding(original, &source.read_projection(&state).nodes)?;
                drop(state);
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
                if proof.original != original {
                    return Err(failure("native offer proof names another original"));
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
        let interface = ArtifactEntry::canonical_artifact_id(
            product
                .module_interface()
                .ok_or_else(|| failure("recovered native offer lacks its canonical interface"))?,
        );
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
        projection: &CompilerInputProjection,
        namespace: &ArtifactView,
    ) -> Option<&QualifiedNativeAvailability> {
        let NativeOfferBody::Full { qualification, .. } = &self.0.body else {
            return None;
        };
        let role = projection.snapshot_roles().get(&self.0.owner)?;
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
            if product.module_interface().is_some_and(|interface| matches!(interface.origin(),
                crate::certified_products::CanonicalOrigin::NativeAuthoredDeclaration { .. })))
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
        projection: &CompilerInputProjection,
    ) -> Result<bool, CompileError> {
        if projection
            .snapshot_roles()
            .get(&self.0.owner)
            .is_some_and(|role| role.interface() == self.0.interface)
        {
            return Ok(true);
        }
        let originals = projection
            .roles()
            .into_iter()
            .filter_map(|role| role.original())
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
        projection: &CompilerInputProjection,
        namespace: &ArtifactView,
    ) -> Result<Option<Self>, CompileError> {
        if self
            .qualified_private_availability(projection, namespace)
            .is_some()
        {
            return Ok(Some(self.clone()));
        }
        // Matching roles propagate the existing opaque issuance directly.
        // Only a different or absent role needs dependency relevance.
        let contributes = || -> Result<bool, CompileError> {
            let originals = projection
                .roles()
                .into_iter()
                .filter_map(|role| role.original())
                .collect::<BTreeSet<_>>();
            Ok(self
                .0
                .custody
                .native_originals
                .iter()
                .any(|original| originals.contains(original)))
        };
        let role = projection.snapshot_roles().get(&self.0.owner);
        let Some(original) = role.and_then(CompilerInputRole::original) else {
            return Ok(contributes()?.then(|| self.clone()));
        };
        let role = role.expect("native role");
        if original != self.0.original || role.interface() != self.0.interface {
            if contributes()? {
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
mod normalized_dependency_tests {
    use super::*;
    use crate::certified_products::tests::{
        original_groups_fixture_with_interface, recovered_witness_fixtures,
    };
    use crate::certified_products::{fixture_finalized_product, PendingImportOwner};
    use crate::declaration_context::{
        ExactCompileContext, ExactDeclarationContext, OriginalCompilerInputs,
    };

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
        let mut selection =
            crate::certified_products::CertifiedSourceSelection::from_compiler_projection(
                &projection,
                &physical.metadata_snapshot(),
                &tidepool_repr::execution_schema::InventoryOperation::new(Default::default()),
            )
            .unwrap();
        let completed = OriginalCompilerInputs::issue_segment_native_availability(
            &mut selection,
            &physical,
            entries,
        )
        .unwrap()
        .issued_artifacts()
        .clone();
        let inputs = OriginalCompilerInputs::from_selection(&selection, &completed).unwrap();
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
                IssuedNativeOffer::issue(offer.original(), &graph, offer.proof().cloned()).unwrap();
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
        let mut selection =
            crate::certified_products::CertifiedSourceSelection::from_compiler_projection(
                &projection,
                &physical.metadata_snapshot(),
                &tidepool_repr::execution_schema::InventoryOperation::new(Default::default()),
            )
            .unwrap();
        let completed = OriginalCompilerInputs::issue_segment_native_availability(
            &mut selection,
            &physical,
            entries,
        )
        .unwrap()
        .issued_artifacts()
        .clone();
        let inputs = OriginalCompilerInputs::from_selection(&selection, &completed).unwrap();
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
            IssuedNativeOffer::issue(offer.original(), &graph, offer.proof().cloned()).unwrap();
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
        let selection =
            crate::certified_products::CertifiedSourceSelection::from_compiler_projection(
                &projection,
                &artifacts.metadata_snapshot(),
                &tidepool_repr::execution_schema::InventoryOperation::new(Default::default()),
            )
            .unwrap();
        let inputs = OriginalCompilerInputs::from_selection(&selection, &artifacts).unwrap();
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
        let sibling = ArtifactEntry::canonical(sibling.module_interface().unwrap().clone());
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
        let broad_offer = IssuedNativeOffer::issue(original, &broad, Some(proof)).unwrap();
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
        let selection =
            crate::certified_products::CertifiedSourceSelection::from_compiler_projection(
                &projection,
                &artifacts.metadata_snapshot(),
                &tidepool_repr::execution_schema::InventoryOperation::new(Default::default()),
            )
            .unwrap();
        let inputs = OriginalCompilerInputs::from_selection(&selection, &artifacts).unwrap();
        let complete = CompilerSourceSupport::close_complete_output(&inputs).unwrap();
        let root_owner = ExactModuleIdentity {
            unit: products[1].owner().unit.clone(),
            module: products[1].owner().module.clone(),
        };
        let interfaces = complete.selected_interfaces().unwrap();
        let types = interfaces
            .support_for_owners(vec![root_owner.clone()])
            .unwrap();
        let retained = types.retain_template_availability(&complete).unwrap();
        assert_eq!(retained.projection().snapshot_roles().len(), 1);
        assert!(retained
            .projection()
            .roles()
            .iter()
            .all(|role| role.original().is_none()));
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
        let repeated = crate::declaration_context::RequestNativeAvailability::issue_checked(
            complete.projection(),
            complete.artifacts(),
            &repeated_offers,
        )
        .unwrap();
        let repeated = complete.retain_normalized_availability(&repeated).unwrap();
        let empty = crate::declaration_context::RequestNativeAvailability::issue_checked(
            complete.projection(),
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
            .groups(
                NativeGroupKey {
                    artifact: hidden_id,
                    original_ordinal: 7
                },
                products[0].owner()
            )
            .unwrap()
            .is_empty());
        assert!(catalog
            .groups(
                NativeGroupKey {
                    artifact: hidden_id,
                    original_ordinal: 9
                },
                products[0].owner()
            )
            .unwrap()
            .is_empty());
        assert!(catalog
            .groups(
                NativeGroupKey {
                    artifact: sibling_id,
                    original_ordinal: 11
                },
                products[2].owner()
            )
            .unwrap()
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
        let root_projection =
            CompilerInputProjection::from_issued_entries(&[entries[&root_id].clone()]).unwrap();
        let mut selection =
            crate::certified_products::CertifiedSourceSelection::from_compiler_projection(
                &root_projection,
                &parent.metadata_snapshot(),
                &tidepool_repr::execution_schema::InventoryOperation::new(Default::default()),
            )
            .unwrap();
        selection
            .finish_new_product_availability(
                &parent.metadata_snapshot(),
                complete.native_availability(),
            )
            .unwrap();
        let canonical = Arc::new(ArtifactEntry::canonical(
            products[0].module_interface().unwrap().clone(),
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
                let mut required = BTreeMap::new();
                acquired
                    .retain_dependency_groups(&entries, &namespace, &mut required)
                    .unwrap();
                assert_eq!(
                    required[&NativeGroupKey {
                        artifact: hidden_id,
                        original_ordinal: 7
                    }],
                    [expected_hidden].into_iter().collect::<imbl::OrdSet<_>>()
                );
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
        let mut fixed = inventory
            .bind_segment(&receiving, vec![], &selection)
            .unwrap();
        assert!(!fixed.view.namespace().contains_key(&hidden_id));
        assert_ne!(
            fixed.view.namespace()[&canonical_id],
            parent.namespace()[&canonical_id]
        );
        let imports = [PendingImportOwner::Source {
            owner: products[0].owner().clone(),
            original_ordinal: 7,
            binder: tidepool_repr::execution_schema::testing::identity(
                "NormalizedHidden",
                "entry_7",
            ),
        }];
        let before_bound = fixed.observed_binding_state();
        let error = inventory
            .admit_bound_target(
                &mut fixed,
                &inventory.empty_view(),
                NativeArtifactDemand::CertifiedTargetImports(&imports),
                vec![],
                &selection,
            )
            .err()
            .expect("a direct private Input demand lacks a root choice or current product");
        assert!(
            matches!(error, CompileError::ArtifactInventory(ref error)
            if error.native_refusal == Some(NativeAdmissionRefusal::CurrentProductNotCompleted {
                original: hidden_id, original_ordinal: 7,
            })),
            "a sole private issued candidate cannot bypass root choice: {error:?}"
        );
        assert_eq!(fixed.observed_binding_state(), before_bound);
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
        let support = crate::declaration_context::issue_fixture_compiler_support(
            projection,
            artifacts.clone(),
        )
        .unwrap();
        selection
            .finish_new_product_availability(
                &artifacts.metadata_snapshot(),
                support.native_availability(),
            )
            .unwrap();
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
        let mut projection = CompilerInputProjection::default();
        projection
            .admit_role(
                entry.descriptor.owner.clone(),
                CompilerInputRole::PublishedSourceOriginal {
                    interface: ArtifactEntry::canonical_artifact_id(
                        original.module_interface().unwrap(),
                    ),
                    original: entry.descriptor.id,
                    source_revision: "pending fixture revision".into(),
                    original_input_identity: "pending fixture input".into(),
                    selection: ExactArtifactSelection::capture(&artifacts),
                    selection_sha256: [0; 32],
                    native_authority: PublicationNativeAuthority::recorded(
                        crate::exact_context::PublicationNativeFacts::maintain(
                            None,
                            crate::exact_context::PublicationNativeMaps {
                                roles: Default::default(),
                                offers: Default::default(),
                                native_availability: Default::default(),
                                input_decisions: Default::default(),
                            },
                        )
                        .unwrap(),
                    ),
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
        let hidden_interface = Arc::new(ArtifactEntry::canonical(
            hidden.module_interface().unwrap().clone(),
        ));
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
