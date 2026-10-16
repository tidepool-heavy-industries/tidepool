use super::*;

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

#[derive(Debug, PartialEq, Eq)]
struct NativeOfferFacts {
    original: ArtifactId,
    interface: ArtifactId,
    owner: ExactModuleIdentity,
    body: NativeOfferBody,
    custody: ArtifactView,
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

impl IssuedNativeOffer {
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
        Ok(Self(Arc::new(NativeOfferFacts {
            original,
            interface,
            owner: entry.descriptor.owner.clone(),
            body,
            custody,
        })))
    }

    pub(crate) fn snapshot_facts(&self) -> crate::exact_context::NativeOfferSnapshotFacts {
        use crate::exact_context::{NativeOfferSnapshotBody, NativeOfferSnapshotFacts};
        NativeOfferSnapshotFacts {
            original: self.0.original,
            interface: self.0.interface,
            owner: self.0.owner.clone(),
            body: match &self.0.body {
                NativeOfferBody::Empty => {
                    NativeOfferSnapshotBody::Empty(ExactArtifactSelection::capture(&self.0.custody))
                }
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
        let exact_empty;
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
            exact_empty = source.select_issued(Vec::new(), custody)?;
            &exact_empty
        } else {
            source
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
            NativeOfferSnapshotBody::Full(_) if !offer.is_full() => {
                return Err(failure("recovered full native offer no longer qualifies"));
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

    pub(crate) fn custody(&self) -> &ArtifactView {
        &self.0.custody
    }

    pub(crate) fn issued_group_carriers(
        &self,
    ) -> Result<BTreeMap<NativeGroupBinding, ArtifactBinding>, CompileError> {
        self.proof()
            .map(|proof| proof.issued_group_carriers(&self.0.custody))
            .transpose()
            .map(Option::unwrap_or_default)
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
            .issued_group_carriers()?
            .keys()
            .any(|group| originals.contains(&group.binding.artifact())))
    }

    /// Current roles may stop offering an original, or stop qualifying its full
    /// census. Neither change authorizes an alternative ambient proof.
    pub(crate) fn within_projection(
        &self,
        projection: &CompilerInputProjection,
        namespace: &ArtifactView,
    ) -> Result<Option<Self>, CompileError> {
        let originals = projection
            .roles()
            .into_iter()
            .filter_map(|role| role.original())
            .collect::<BTreeSet<_>>();
        let contributes = self
            .issued_group_carriers()?
            .keys()
            .any(|group| originals.contains(&group.binding.artifact()));
        let role = projection.snapshot_roles().get(&self.0.owner);
        let Some(original) = role.and_then(CompilerInputRole::original) else {
            return Ok(contributes.then(|| self.clone()));
        };
        let role = role.expect("native role");
        if original != self.0.original || role.interface() != self.0.interface {
            if contributes {
                return Ok(Some(self.clone()));
            }
            return Err(failure(
                "native offer conflicts with selected compiler product or interface",
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
