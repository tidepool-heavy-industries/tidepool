use super::*;

/// Fresh bodies can depend on an available group without selecting it for
/// execution. Resolve only their certified imports from the supplied offers;
/// proof roots remain separate from these dependency choices.
pub(super) fn bind_fresh_product_dependencies(
    state: &InventoryState,
    parent: &ArtifactView,
    consumed_namespace: &BTreeMap<ExactModuleIdentity, Arc<ArtifactEntry>>,
    entries: &BTreeMap<ArtifactId, Arc<ArtifactEntry>>,
    fresh: &BTreeSet<NativeGroupKey>,
    offers: &[IssuedNativeOffer],
    required: &mut BTreeMap<NativeGroupKey, imbl::OrdSet<NativeGroupBinding>>,
) -> Result<(), CompileError> {
    let namespace = parent.namespace();
    let mut wanted = BTreeSet::new();
    for group in fresh {
        let entry = &entries[&group.artifact];
        for (owner, dependency) in entry.native_requirements_for(group.original_ordinal) {
            let ArtifactDependency::NativeGroup {
                required_ordinal, ..
            } = dependency
            else {
                continue;
            };
            let Some(owner) = entry.native_owners.get(owner) else {
                continue;
            };
            let mut matching = entries.values().filter(|entry| {
                matches!(&entry.payload, ArtifactPayload::Original(product)
                    if NativeOwnerKey::from_owner(product.owner()) == *owner)
            });
            let Some(target) = matching.next() else {
                continue;
            };
            if matching.next().is_some() {
                return Err(admission_failure(
                    ArtifactInventoryFailure::BindingAmbiguity {
                        artifact: target.descriptor.id,
                    },
                ));
            }
            let target = NativeGroupKey {
                artifact: target.descriptor.id,
                original_ordinal: *required_ordinal,
            };
            if !fresh.contains(&target) && !required.contains_key(&target) {
                wanted.insert(target);
            }
        }
    }
    if wanted.is_empty() {
        return Ok(());
    }
    let mut offered = imbl::OrdSet::new();
    for offer in offers {
        for key in &wanted {
            for group in offer.group_bindings(*key) {
                let node = InventoryNodeKey::Group(group);
                if !state.contains_node(&node) {
                    return Err(failure(
                        "issued dependency scope is outside receiving inventory",
                    ));
                }
                offered.insert(node);
            }
        }
    }
    let index = NativeSelectionIndex::build(state, &offered);
    for key in wanted {
        let mut candidates = BTreeSet::new();
        let ArtifactPayload::Original(product) = &entries[&key.artifact].payload else {
            unreachable!("certified native dependency has an original owner")
        };
        let canonical = ArtifactEntry::canonical(product.module_interface().unwrap().clone())
            .descriptor
            .id;
        if let Some(binding) = namespace.get(&canonical) {
            for (carrier, ordinal) in index.groups.keys() {
                if carrier.artifact != key.artifact
                    || *ordinal != key.original_ordinal
                    || namespace
                        .get(&key.artifact)
                        .is_some_and(|selected| selected != carrier)
                {
                    continue;
                }
                if state
                    .outgoing(InventoryNodeKey::Artifact(*carrier))
                    .any(|(target, edge)| {
                        target == InventoryNodeKey::Artifact(*binding)
                            && *edge == ArtifactDependency::Interface
                    })
                {
                    candidates.insert(index.resolve(*carrier, *ordinal)?);
                }
            }
        }
        let Some(group) = candidates.first().copied() else {
            return Err(admission_failure(
                ArtifactInventoryFailure::NativeGroupUnavailable {
                    artifact: key.artifact,
                    original_ordinal: key.original_ordinal,
                },
            ));
        };
        if candidates.len() != 1 {
            return Err(admission_failure(
                ArtifactInventoryFailure::BindingAmbiguity {
                    artifact: key.artifact,
                },
            ));
        }
        // This group's immutable edges own its historical children. Do not
        // resolve each child again against the receiving namespace.
        for node in admitted_closure(state, [InventoryNodeKey::Group(group)].into_iter()) {
            if let InventoryNodeKey::Group(group) = node {
                validate_consumed_native_namespace(
                    &entries[&group.binding.artifact()],
                    consumed_namespace,
                )?;
                required
                    .entry(NativeGroupKey {
                        artifact: group.binding.artifact(),
                        original_ordinal: group.original_ordinal,
                    })
                    .or_default()
                    .insert(group);
            }
        }
    }
    Ok(())
}

/// Consuming exact native support must respect a selected compiler role. An
/// absent role grants no namespace intent; an incompatible selected role cannot
/// be bypassed by retaining a historical implementation beside it.
pub(crate) fn validate_consumed_native_namespace(
    dependency: &ArtifactEntry,
    namespace: &BTreeMap<ExactModuleIdentity, Arc<ArtifactEntry>>,
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
        return Err(failure(
            "issued native dependency conflicts with request interface",
        ));
    }
    Ok(())
}

/// Exact lifetime custody, without a derived artifact-view projection.
/// Only the inventory's checked issuance or an already authenticated view can
/// construct this owner; immutable proof bytes alone are insufficient.
#[derive(Clone)]
struct NativeOfferCustody {
    lease: Arc<ViewLease>,
    reachable: imbl::OrdSet<ArtifactBindingNode>,
    native_originals: BTreeSet<ArtifactId>,
    #[cfg(test)]
    observed: Arc<OnceLock<ArtifactView>>,
}

impl NativeOfferCustody {
    fn from_checked_nodes(
        state: &InventoryState,
        lease: Arc<ViewLease>,
        reachable: imbl::OrdSet<ArtifactBindingNode>,
    ) -> Result<Self, CompileError> {
        let mut native_originals = BTreeSet::new();
        for node in &reachable {
            let ArtifactBindingNode::Group(group) = node else {
                continue;
            };
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
        Ok(Self {
            lease,
            reachable,
            native_originals,
            #[cfg(test)]
            observed: Arc::new(OnceLock::new()),
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
}

impl NativeOfferContribution {
    pub(crate) fn offer(&self) -> &IssuedNativeOffer {
        &self.offer
    }

    pub(crate) fn originals(&self) -> &BTreeSet<ArtifactId> {
        &self.originals
    }

    pub(crate) fn permits(&self, key: NativeGroupKey) -> bool {
        self.offer
            .group_bindings(key)
            .any(|group| self.reachable.contains(&ArtifactBindingNode::Group(group)))
    }

    pub(crate) fn ordinals(&self, original: ArtifactId) -> impl Iterator<Item = u32> + '_ {
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
                    Some(group.original_ordinal)
                }
                _ => None,
            })
    }
}

#[derive(Debug)]
struct NativeOfferFacts {
    original: ArtifactId,
    interface: ArtifactId,
    owner: ExactModuleIdentity,
    body: NativeOfferBody,
    custody: NativeOfferCustody,
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
        })
    }

    pub(crate) fn validate(
        self,
        baseline: &ArtifactView,
        baseline_groups: &BTreeSet<NativeGroupKey>,
        preferred: &[NativeOriginalRequirement],
        validation: &mut crate::recovery_artifacts::PackageInterfaceValidation,
    ) -> Result<Vec<IssuedNativeOffer>, CompileError> {
        let originals = self
            .entries
            .iter()
            .filter_map(|(id, entry)| match &entry.payload {
                ArtifactPayload::Original(product) => Some((*id, product)),
                _ => None,
            })
            .collect::<BTreeMap<_, _>>();
        let mut advertised = baseline_groups.clone();
        let mut qualifications = BTreeMap::new();
        for proof in preferred {
            if let Some(qualification) = proof.qualify_availability(baseline)? {
                if qualifications
                    .insert(qualification.original(), qualification.clone())
                    .is_some_and(|prior| prior != qualification)
                {
                    return Err(admission_failure(
                        ArtifactInventoryFailure::BindingAmbiguity {
                            artifact: qualification.original(),
                        },
                    ));
                }
            }
        }
        for offer in &self.offers {
            let product = originals
                .get(&offer.original())
                .ok_or_else(|| failure("private original is outside retained custody"))?;
            advertised.extend(
                crate::certified_products::original_available_groups(product)
                    .map_err(|error| CompileError::CompilerEvidence(Box::new(error)))?
                    .map(|group| NativeGroupKey {
                        artifact: offer.original(),
                        original_ordinal: group.original_ordinal(),
                    }),
            );
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
        Ok(self.offers)
    }
}

impl IssuedNativeOffer {
    /// Complete availability is issued directly by the checked scope solver.
    /// A complete census in bytes alone cannot enter this constructor.
    fn issue_complete_custody(
        admitted: IssuedArtifactCustody,
        originals: &[ArtifactId],
    ) -> Result<Vec<Self>, CompileError> {
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
            let interface = ArtifactEntry::canonical(
                product
                    .module_interface()
                    .ok_or_else(|| failure("complete availability lacks canonical interface"))?
                    .clone(),
            )
            .descriptor
            .id;
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
            let payloads = artifact_ids(&nodes)
                .into_iter()
                .map(|id| (id, Arc::clone(&source.payloads[&id])))
                .collect();
            let lease = Arc::new(ViewLease {
                admission_roots: Default::default(),
                selection: Arc::new(custody_selection),
                payloads,
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
            offers.push(Self(Arc::new(NativeOfferFacts {
                original,
                interface,
                owner: entry.descriptor.owner.clone(),
                body,
                custody: NativeOfferCustody::from_checked_nodes(&state, lease, nodes)?,
            })));
        }
        drop(state);
        let mut state = source.inventory.0.lock().expect("inventory lock");
        for offer in &offers {
            let lease = offer.custody_lease();
            state
                .views
                .push((Arc::downgrade(lease), Arc::clone(&lease.selection)));
        }
        Ok(offers)
    }

    pub(crate) fn full_qualification(&self) -> Option<&QualifiedNativeAvailability> {
        match &self.0.body {
            NativeOfferBody::Full { qualification, .. } => Some(qualification),
            _ => None,
        }
    }

    /// The same immutable issuance also retains the same acquisition owners.
    /// Semantic offer equality alone does not establish this relationship.
    pub(super) fn same_issuance(&self, other: &Self) -> bool {
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
        let interface = ArtifactEntry::canonical(
            product
                .module_interface()
                .ok_or_else(|| failure("native offer lacks its canonical interface"))?
                .clone(),
        )
        .descriptor
        .id;
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
        let custody = {
            let state = custody.lease.inventory.0.lock().expect("inventory lock");
            NativeOfferCustody::from_checked_nodes(
                &state,
                Arc::clone(&custody.lease),
                custody.read_projection(&state).nodes.clone(),
            )?
        };
        Ok(Self(Arc::new(NativeOfferFacts {
            original,
            interface,
            owner: entry.descriptor.owner.clone(),
            body,
            custody,
        })))
    }

    /// Borrow durable custody facts without constructing a selected view.
    /// Reopening these facts still requires the recovery admission boundary.
    pub(crate) fn custody_snapshot(&self) -> &Arc<crate::exact_context::ExactSelectionSnapshot> {
        &self.0.custody.lease.selection
    }

    pub(crate) fn snapshot_facts(&self) -> crate::exact_context::NativeOfferSnapshotFacts {
        use crate::exact_context::{NativeOfferSnapshotBody, NativeOfferSnapshotFacts};
        NativeOfferSnapshotFacts {
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
        }
    }

    /// Restore the recorded availability class against authenticated recovered
    /// custody. Successful qualification cannot upgrade a partial issuance.
    pub(crate) fn restore_snapshot(
        facts: &crate::exact_context::NativeOfferSnapshotFacts,
        source: &ArtifactView,
    ) -> Result<Self, CompileError> {
        use crate::exact_context::NativeOfferSnapshotBody;
        let exact_source;
        let source = if let NativeOfferSnapshotBody::Empty(custody) = &facts.body {
            let graph = custody.graph_selection();
            if !graph.roots.is_empty()
                || !graph.namespace.is_empty()
                || !matches!(graph.custody_roots.as_slice(), [ArtifactBindingNode::Artifact(binding)] if binding.artifact == facts.original)
            {
                return Err(failure(
                    "recovered empty offer lacks exact original custody",
                ));
            }
            exact_source = source.select_issued(Vec::new(), custody)?;
            &exact_source
        } else {
            let selection = match &facts.body {
                NativeOfferSnapshotBody::Partial(selection)
                | NativeOfferSnapshotBody::Full(selection) => selection,
                NativeOfferSnapshotBody::Empty(_) => unreachable!(),
            };
            exact_source = source.select_issued(Vec::new(), selection)?;
            &exact_source
        };
        let selection = match &facts.body {
            NativeOfferSnapshotBody::Empty(_) => None,
            NativeOfferSnapshotBody::Partial(selection)
            | NativeOfferSnapshotBody::Full(selection) => Some(selection.clone()),
        };
        let mut offer = Self::issue(
            facts.original,
            source,
            selection.map(|selection| NativeOriginalRequirement {
                original: facts.original,
                selection,
            }),
        )?;
        if offer.0.interface != facts.interface || offer.0.owner != facts.owner {
            return Err(failure(
                "recovered native offer differs from its original interface or owner",
            ));
        }
        match &facts.body {
            NativeOfferSnapshotBody::Full(_) => {
                let proof = offer.proof().expect("full snapshot retains proof").clone();
                let qualification = proof
                    .qualify_issued_availability(source)?
                    .ok_or_else(|| failure("recovered full native offer no longer qualifies"))?;
                offer = Self(Arc::new(NativeOfferFacts {
                    original: offer.0.original,
                    interface: offer.0.interface,
                    owner: offer.0.owner.clone(),
                    body: NativeOfferBody::Full {
                        proof,
                        qualification,
                    },
                    custody: offer.0.custody.clone(),
                }));
            }
            NativeOfferSnapshotBody::Partial(_) => {
                let proof = offer
                    .proof()
                    .expect("partial snapshot retains proof")
                    .clone();
                offer = Self(Arc::new(NativeOfferFacts {
                    original: offer.0.original,
                    interface: offer.0.interface,
                    owner: offer.0.owner.clone(),
                    body: NativeOfferBody::Partial(proof),
                    custody: offer.0.custody.clone(),
                }));
            }
            _ => {}
        }
        Ok(offer)
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

    #[cfg(test)]
    pub(crate) fn custody(&self) -> &ArtifactView {
        self.0
            .custody
            .observed
            .get_or_init(|| ArtifactView::from_custody(Arc::clone(&self.0.custody.lease)))
    }

    pub(super) fn custody_lease(&self) -> &Arc<ViewLease> {
        &self.0.custody.lease
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
        }))))
    }
}
