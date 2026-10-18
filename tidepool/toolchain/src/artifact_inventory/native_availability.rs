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
    pub(super) bindings: BTreeMap<NativeGroupKey, NativeGroupBinding>,
    pub(super) offers: Vec<IssuedNativeOffer>,
}

/// Bind demanded input bodies through their original opaque owner. Carrier-only
/// views do not become proof owners, and availability does not add demand roots.
pub(super) fn bind_certified_input_demand(
    state: &mut InventoryState,
    parent: &ArtifactView,
    entries: &BTreeMap<ArtifactId, Arc<ArtifactEntry>>,
    incoming: BTreeMap<ArtifactId, Arc<ArtifactEntry>>,
    demanded: &BTreeSet<NativeGroupKey>,
    selection: &crate::certified_products::CertifiedSourceSelection,
) -> Result<CertifiedNativeDemandAcquisition, CompileError> {
    bind_input_demand(
        state,
        parent,
        entries,
        incoming,
        demanded,
        NativeDemandSource::Certified(selection),
    )
}

#[derive(Clone, Copy)]
enum NativeDemandSource<'a> {
    Certified(&'a crate::certified_products::CertifiedSourceSelection),
    Catalog(&'a NativeDependencyCatalog<'a>),
}

fn bind_input_demand(
    state: &mut InventoryState,
    parent: &ArtifactView,
    entries: &BTreeMap<ArtifactId, Arc<ArtifactEntry>>,
    mut incoming: BTreeMap<ArtifactId, Arc<ArtifactEntry>>,
    demanded: &BTreeSet<NativeGroupKey>,
    source: NativeDemandSource<'_>,
) -> Result<CertifiedNativeDemandAcquisition, CompileError> {
    let namespace = parent.namespace();
    let mut retained_offers = Vec::new();
    let mut prebound = BTreeMap::new();
    for key in demanded {
        let entry = incoming
            .get(&key.artifact)
            .or_else(|| entries.get(&key.artifact))
            .ok_or_else(|| {
                admission_failure(ArtifactInventoryFailure::NativeRootOutsideView {
                    artifact: key.artifact,
                })
            })?;
        let ArtifactPayload::Original(product) = &entry.payload else {
            return Err(failure("native demand lacks original payload"));
        };
        let mut supports = match source {
            NativeDemandSource::Certified(selection) => {
                if !selection.permits_native_group(
                    key.artifact,
                    product.owner(),
                    key.original_ordinal,
                ) {
                    return Err(admission_failure(
                        ArtifactInventoryFailure::NativeGroupUnavailable {
                            artifact: key.artifact,
                            original_ordinal: key.original_ordinal,
                        },
                    ));
                }
                let resolution = selection
                    .native_group_candidates(*key, product.owner())
                    .map_err(|error| CompileError::CompilerEvidence(Box::new(error)))?;
                let crate::certified_products::NativeGroupCandidates::IssuedAvailability(
                    candidates,
                ) = resolution
                else {
                    continue;
                };
                candidates
                    .into_iter()
                    .map(|group| {
                        selection
                            .native_group_support(*key, product.owner(), group)
                            .map(|support| (group, support))
                            .ok_or_else(|| {
                                admission_failure(
                                    ArtifactInventoryFailure::NativeGroupUnavailable {
                                        artifact: key.artifact,
                                        original_ordinal: key.original_ordinal,
                                    },
                                )
                            })
                    })
                    .collect::<Result<Vec<_>, _>>()?
            }
            NativeDemandSource::Catalog(catalog) => catalog
                .group_supports(*key, product.owner())
                .into_iter()
                .map(|(group, offer)| {
                    (
                        group,
                        crate::certified_products::CertifiedNativeGroupSupport::Issued(offer),
                    )
                })
                .collect(),
        };
        if let NativeDemandSource::Catalog(catalog) = source {
            if let InputNativeDecision::IssuedChoices(choices) =
                catalog.decision(&entry.descriptor.owner)
            {
                if let Some(chosen) = choices.get(&key.original_ordinal) {
                    supports.retain(|(group, _)| group == chosen);
                }
            }
        }
        let canonical = ArtifactEntry::canonical(
            product
                .module_interface()
                .ok_or_else(|| failure("native canonical carrier missing"))?
                .clone(),
        )
        .descriptor
        .id;
        let mut compatible = Vec::new();
        for (group, support) in supports {
            let node = InventoryNodeKey::Group(group);
            let (owned, payloads, retained_offer) = match support {
                crate::certified_products::CertifiedNativeGroupSupport::Issued(offer) => {
                    if !offer.contains_checked_group(group) {
                        return Err(failure("issued native contribution lacks demanded group"));
                    }
                    (
                        offer
                            .custody_snapshot()
                            .nodes_from_seeds([node])
                            .map_err(|_| failure("issued native dependency closure missing"))?,
                        &offer.custody_lease().payloads,
                        Some(offer),
                    )
                }
                crate::certified_products::CertifiedNativeGroupSupport::NewProductDependency(
                    proof,
                ) => {
                    validate_issued_selection(
                        state,
                        &parent.read_projection(state).nodes,
                        proof.selection.graph_selection(),
                    )?;
                    (
                        admitted_closure(state, [node].into_iter())
                            .iter()
                            .copied()
                            .collect(),
                        &parent.lease.payloads,
                        None,
                    )
                }
            };
            // Candidate compatibility is read from its issuing snapshot. A
            // receiving inventory need not yet know these exact historical scopes.
            let outgoing = |source: InventoryNodeKey| {
                if let Some(offer) = retained_offer {
                    let snapshot = offer.custody_snapshot();
                    let proof = snapshot
                        .scopes
                        .get(&source.selection())
                        .expect("checked closure retains each scope")
                        .proof();
                    let start = proof
                        .dependencies
                        .partition_point(|(from, _, _)| *from < source);
                    proof.dependencies[start..]
                        .iter()
                        .take_while(|(from, _, _)| *from == source)
                        .map(|(_, target, edge)| (*target, edge.clone()))
                        .collect::<Vec<_>>()
                } else {
                    state
                        .outgoing(source)
                        .map(|(target, edge)| (target, edge.clone()))
                        .collect()
                }
            };
            let carriers = outgoing(node)
                .into_iter()
                .filter_map(|(target, edge)| match target {
                    InventoryNodeKey::Artifact(carrier)
                        if carrier.artifact == key.artifact
                            && edge == ArtifactDependency::Interface =>
                    {
                        Some(carrier)
                    }
                    _ => None,
                })
                .collect::<BTreeSet<_>>();
            if carriers.len() != 1 {
                return Err(admission_failure(
                    ArtifactInventoryFailure::BindingAmbiguity {
                        artifact: key.artifact,
                    },
                ));
            }
            let carrier = *carriers.first().unwrap();
            if namespace
                .get(&key.artifact)
                .is_some_and(|selected| *selected != carrier)
            {
                continue;
            }
            if matches!(source, NativeDemandSource::Catalog(_))
                && !namespace.contains_key(&canonical)
            {
                continue;
            }
            if outgoing(InventoryNodeKey::Artifact(carrier))
                .into_iter()
                .any(|(target, edge)| {
                    matches!(target, InventoryNodeKey::Artifact(binding)
                    if binding.artifact == canonical
                    && namespace.get(&canonical).is_none_or(|selected| *selected == binding))
                        && edge == ArtifactDependency::Interface
                })
            {
                compatible.push((group, payloads, owned, retained_offer));
            }
        }
        if compatible.len() > 1 {
            return Err(admission_failure(
                ArtifactInventoryFailure::BindingAmbiguity {
                    artifact: key.artifact,
                },
            ));
        }
        let Some((group, payloads, owned, retained_offer)) = compatible.pop() else {
            return Err(admission_failure(
                ArtifactInventoryFailure::NativeGroupUnavailable {
                    artifact: key.artifact,
                    original_ordinal: key.original_ordinal,
                },
            ));
        };
        if let Some(offer) = retained_offer {
            let snapshot = offer.custody_snapshot();
            let mut pending = owned
                .iter()
                .map(|node| node.selection())
                .collect::<Vec<_>>();
            let mut scopes = BTreeMap::new();
            while let Some(scope) = pending.pop() {
                if scopes.contains_key(&scope) {
                    continue;
                }
                let proof = snapshot
                    .scopes
                    .get(&scope)
                    .ok_or_else(|| failure("demanded closure lacks issued scope"))?
                    .proof();
                pending.extend(proof.scope_dependencies());
                scopes.insert(scope, Arc::clone(proof));
            }
            // Atomic scope facts can include unselected sibling rows. Import
            // only the chosen closure's required scopes, never competing offers.
            // These authenticated facts remain memoized for this inventory even
            // if later admission refuses; they publish no selection or payload rights.
            let prepared = scope_proof::prepare_install(state, scopes.into_values())?;
            scope_proof::install(state, prepared);
        }
        if !state.contains_node(&InventoryNodeKey::Group(group))
            || admitted_closure(state, [InventoryNodeKey::Group(group)].into_iter())
                .iter()
                .copied()
                .collect::<BTreeSet<_>>()
                != owned
        {
            return Err(admission_failure(
                ArtifactInventoryFailure::NativeRootOutsideView {
                    artifact: key.artifact,
                },
            ));
        }
        // Retain just this demanded immutable closure. The offer remains the
        // authority for membership; no sibling proof roots enter selection.
        for node in owned {
            if let InventoryNodeKey::Artifact(binding) = node {
                let payload = payloads
                    .get(&binding.artifact)
                    .ok_or_else(|| failure("issued native dependency payload missing"))?;
                let retained = match incoming
                    .get(&binding.artifact)
                    .or_else(|| entries.get(&binding.artifact))
                {
                    Some(previous) => {
                        ArtifactEntry::merge_catalog_input_custody(previous, payload)?
                    }
                    None => Arc::clone(payload),
                };
                incoming.insert(binding.artifact, retained);
            }
        }
        prebound.insert(*key, group);
        if let Some(offer) = retained_offer {
            retained_offers.push(offer.clone());
        }
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
    entries: &BTreeMap<ArtifactId, Arc<ArtifactEntry>>,
    fresh: &BTreeSet<NativeGroupKey>,
    dependencies: &NativeDependencyCatalog<'_>,
    required: &mut BTreeMap<NativeGroupKey, imbl::OrdSet<NativeGroupBinding>>,
) -> Result<CertifiedNativeDemandAcquisition, CompileError> {
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
    let acquired = bind_input_demand(
        state,
        parent,
        entries,
        BTreeMap::new(),
        &wanted,
        NativeDemandSource::Catalog(dependencies),
    )?;
    for group in acquired.bindings.values() {
        // Historical children follow this exact acquired owner. These are
        // dependency edges for new bodies, never executable request roots.
        for node in admitted_closure(state, [InventoryNodeKey::Group(*group)].into_iter()) {
            if let InventoryNodeKey::Group(group) = node {
                let entry = acquired
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
                    .insert(group);
            }
        }
    }
    Ok(acquired)
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
    artifact_carriers: BTreeSet<ArtifactBinding>,
}

impl NativeOfferCustody {
    fn from_checked_nodes(
        lease: Arc<ViewLease>,
        reachable: imbl::OrdSet<ArtifactBindingNode>,
    ) -> Result<Self, CompileError> {
        let mut native_originals = BTreeSet::new();
        let mut artifact_carriers = BTreeSet::new();
        let state = lease.inventory.0.lock().expect("inventory lock");
        for node in &reachable {
            let group = match node {
                ArtifactBindingNode::Artifact(carrier) => {
                    artifact_carriers.insert(*carrier);
                    continue;
                }
                ArtifactBindingNode::Group(group) => group,
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
        let available_groups = native_groups(&reachable);
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

impl NativeOfferContribution {
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
                offer: self.offer.snapshot_facts().identity()?,
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
        if offer.snapshot_facts().identity()? != facts.offer
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
        let state = self
            .offer
            .0
            .custody
            .lease
            .inventory
            .0
            .lock()
            .expect("inventory lock");
        let reachable = admitted_closure(
            &state,
            roots
                .iter()
                .copied()
                .filter(|group| self.reachable.contains(&ArtifactBindingNode::Group(*group)))
                .map(ArtifactBindingNode::Group),
        );
        if !reachable.iter().all(|node| self.reachable.contains(node)) {
            return Err(failure("restricted native contribution exceeds its issuer"));
        }
        drop(state);
        if reachable == self.reachable {
            return Ok(Arc::clone(self));
        }
        let originals = reachable
            .iter()
            .filter_map(|node| match node {
                ArtifactBindingNode::Group(group) => Some(group.binding.artifact()),
                _ => None,
            })
            .collect();
        Ok(Arc::new(Self {
            offer: self.offer.clone(),
            reachable,
            originals,
            identity: OnceLock::new(),
        }))
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
        self.offer
            .group_bindings(key)
            .filter(|group| self.reachable.contains(&ArtifactBindingNode::Group(*group)))
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

    pub(crate) fn validate(
        self,
        baseline_groups: &BTreeSet<NativeGroupKey>,
        preferred: &[QualifiedNativeAvailability],
        validation: &mut crate::recovery_artifacts::PackageInterfaceValidation,
    ) -> Result<ValidatedFreshNativeAvailability, CompileError> {
        let advertised = self.advertised_groups(baseline_groups)?;
        self.validate_advertised(advertised, preferred, validation)
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
            offers.push((
                original,
                interface,
                entry.descriptor.owner.clone(),
                body,
                lease,
                nodes,
            ));
        }
        drop(state);
        offers
            .into_iter()
            .map(|(original, interface, owner, body, lease, nodes)| {
                Ok(Self(Arc::new(NativeOfferFacts {
                    original,
                    interface,
                    owner,
                    body,
                    custody: NativeOfferCustody::from_checked_nodes(lease, nodes)?,
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
        let reachable = {
            let state = custody.lease.inventory.0.lock().expect("inventory lock");
            custody.read_projection(&state).nodes.clone()
        };
        let custody =
            NativeOfferCustody::from_checked_nodes(Arc::clone(&custody.lease), reachable)?;
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
        let interface = ArtifactEntry::canonical(
            product
                .module_interface()
                .ok_or_else(|| failure("recovered native offer lacks its canonical interface"))?
                .clone(),
        )
        .descriptor
        .id;
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
        let custody = NativeOfferCustody::from_checked_nodes(lease, nodes.into_iter().collect())?;
        Ok(Self(Arc::new(NativeOfferFacts {
            original: facts.original,
            interface,
            owner: facts.owner.clone(),
            body,
            custody,
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
        let inputs = OriginalCompilerInputs::from_checked_support_products(
            &CompilerSourceSupport::empty(),
            producer.sha256(),
            &products,
        )
        .unwrap();
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
        let inputs = OriginalCompilerInputs::from_checked_support_products(
            &CompilerSourceSupport::empty(),
            producer.sha256(),
            &products,
        )
        .unwrap();
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
    }
}
