//! Compiler input roles are issued independently of retained native custody.
//! Serialized roles become authority only after checking their exact originals.

use super::*;

/// Persisted input-role facts. The interface is exact type custody; an original
/// offer additionally grants reuse of one authenticated immutable implementation.
/// Constructing or decoding this record grants neither role.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CompilerInputRole {
    InterfaceOnly {
        interface: ArtifactId,
    },
    ReusableOriginal {
        interface: ArtifactId,
        original: ArtifactId,
    },
    PublishedSourceOriginal {
        interface: ArtifactId,
        original: ArtifactId,
        source_revision: String,
        original_input_identity: String,
        selection: ExactArtifactSelection,
        selection_sha256: [u8; 32],
    },
}

impl PartialEq for CompilerInputRole {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::InterfaceOnly { interface: left }, Self::InterfaceOnly { interface: right }) => {
                left == right
            }
            (
                Self::ReusableOriginal {
                    interface: li,
                    original: lo,
                },
                Self::ReusableOriginal {
                    interface: ri,
                    original: ro,
                },
            ) => li == ri && lo == ro,
            (
                Self::PublishedSourceOriginal {
                    interface: li,
                    original: lo,
                    source_revision: lr,
                    original_input_identity: lp,
                    selection: ls,
                    selection_sha256: ld,
                },
                Self::PublishedSourceOriginal {
                    interface: ri,
                    original: ro,
                    source_revision: rr,
                    original_input_identity: rp,
                    selection: rs,
                    selection_sha256: rd,
                },
            ) => {
                li == ri && lo == ro && lr == rr && lp == rp && ld == rd
                // Persistent-map equality covers available proof facts too.
                // Selected semantic identity is observed separately below.
                && (Arc::ptr_eq(ls.snapshot(), rs.snapshot()) || ls.snapshot() == rs.snapshot())
            }
            _ => false,
        }
    }
}
impl Eq for CompilerInputRole {}

impl CompilerInputRole {
    pub fn interface(&self) -> ArtifactId {
        match self {
            Self::InterfaceOnly { interface }
            | Self::ReusableOriginal { interface, .. }
            | Self::PublishedSourceOriginal { interface, .. } => *interface,
        }
    }

    pub fn original(&self) -> Option<ArtifactId> {
        match self {
            Self::InterfaceOnly { .. } => None,
            Self::ReusableOriginal { original, .. }
            | Self::PublishedSourceOriginal { original, .. } => Some(*original),
        }
    }

    pub fn is_published_source_original(&self) -> bool {
        matches!(self, Self::PublishedSourceOriginal { .. })
    }

    fn semantic_observation(&self) -> Result<CompilerRoleObservation<'_>, CompileError> {
        Ok(match self {
            Self::InterfaceOnly { interface } => CompilerRoleObservation::InterfaceOnly {
                interface: *interface,
            },
            Self::ReusableOriginal {
                interface,
                original,
            } => CompilerRoleObservation::ReusableOriginal {
                interface: *interface,
                original: *original,
            },
            Self::PublishedSourceOriginal {
                interface,
                original,
                source_revision,
                original_input_identity,
                selection,
                selection_sha256,
            } => CompilerRoleObservation::PublishedSourceOriginal {
                interface: *interface,
                original: *original,
                source_revision,
                original_input_identity,
                selection: selection.observation_digest()?,
                selection_sha256: *selection_sha256,
            },
        })
    }
}

/// Context identity observes selected semantics without encoding proof transport.
/// The full immutable selection remains with its owning role and durable root.
#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum CompilerRoleObservation<'a> {
    InterfaceOnly {
        interface: ArtifactId,
    },
    ReusableOriginal {
        interface: ArtifactId,
        original: ArtifactId,
    },
    PublishedSourceOriginal {
        interface: ArtifactId,
        original: ArtifactId,
        source_revision: &'a str,
        original_input_identity: &'a str,
        selection: ExactArtifactSelectionDigest,
        selection_sha256: [u8; 32],
    },
}

/// One compiler namespace selected by accepted declarations and import proofs.
/// Availability and executable native demand remain in the retained view.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CompilerInputProjection {
    roles: imbl::OrdMap<ExactModuleIdentity, CompilerInputRole>,
}

/// Matched compiler roles and exact custody selected by their existing issuer.
/// Required type interfaces do not grant lexical imports or native body demand.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CompilerSourceSupport {
    projection: CompilerInputProjection,
    artifacts: ArtifactView,
    native_offers: Arc<[IssuedNativeOffer]>,
    selected: Arc<BTreeMap<ExactModuleIdentity, ResolvedCompilerRole>>,
    input_decisions: IssuedInputNativeDecisions,
}

/// A recorded choice is not authority until its role and exact Group references
/// have been authenticated with the source acquisition owner.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RecordedInputNativeDecision {
    pub(crate) role: CompilerInputRole,
    pub(crate) groups: imbl::OrdMap<u32, NativeGroupBinding>,
}

/// Presence includes an explicitly empty choice. Absence leaves the original
/// under its existing input policy; neither case advertises native availability.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct IssuedInputNativeDecisions {
    pub(super) facts: imbl::OrdMap<ExactModuleIdentity, RecordedInputNativeDecision>,
}

/// This is a carried choice constraint, never native availability or demand.
pub(crate) enum InputNativeDecision<'a> {
    UnissuedPolicy,
    IssuedChoices(&'a imbl::OrdMap<u32, NativeGroupBinding>),
}

impl IssuedInputNativeDecisions {
    pub(crate) fn for_role(
        &self,
        owner: &ExactModuleIdentity,
        role: &CompilerInputRole,
    ) -> InputNativeDecision<'_> {
        match self.facts.get(owner).filter(|fact| &fact.role == role) {
            Some(fact) => InputNativeDecision::IssuedChoices(&fact.groups),
            None => InputNativeDecision::UnissuedPolicy,
        }
    }

    pub(crate) fn from_certified_continuation(
        selection: &crate::certified_products::CertifiedSourceSelection,
        projection: &CompilerInputProjection,
    ) -> Result<Self, CompileError> {
        use crate::certified_products::{CertificationError, InputDecisionRefusal};
        let mut result = Self::default();
        for (owner, role) in projection.snapshot_roles() {
            let Some(original) = role.original() else {
                continue;
            };
            let Some((source, native_owner)) = selection
                .continuation_input(owner)
                .map_err(|error| CompileError::CompilerEvidence(Box::new(error)))?
            else {
                continue;
            };
            let refusal = |reason| {
                CompileError::CompilerEvidence(Box::new(CertificationError::InputDecision {
                    owner: owner.clone(),
                    reason,
                }))
            };
            let choices = match source
                .checked_native_input_decision(original, role.interface(), native_owner)
                .map_err(refusal)?
            {
                InputNativeDecision::IssuedChoices(choices) => choices,
                InputNativeDecision::UnissuedPolicy => {
                    return Err(refusal(InputDecisionRefusal::MissingDecision))
                }
            };
            if !matches!(role, CompilerInputRole::ReusableOriginal { .. }) {
                return Err(refusal(InputDecisionRefusal::RoleMismatch));
            }
            result.facts.insert(
                owner.clone(),
                RecordedInputNativeDecision {
                    role: role.clone(),
                    groups: choices.clone(),
                },
            );
        }
        Ok(result)
    }

    pub(crate) fn recorded_facts(
        &self,
    ) -> &imbl::OrdMap<ExactModuleIdentity, RecordedInputNativeDecision> {
        &self.facts
    }

    fn for_exact_roles(&self, projection: &CompilerInputProjection) -> Self {
        Self {
            facts: self
                .facts
                .iter()
                .filter(|(owner, fact)| projection.snapshot_roles().get(*owner) == Some(&fact.role))
                .map(|(owner, fact)| (owner.clone(), fact.clone()))
                .collect(),
        }
    }

    pub(crate) fn restricted(
        &self,
        projection: &CompilerInputProjection,
        view: &ArtifactView,
    ) -> Self {
        let roots = view.roots();
        Self {
            facts: self
                .facts
                .iter()
                .filter(|(owner, fact)| projection.snapshot_roles().get(*owner) == Some(&fact.role))
                .map(|(owner, fact)| {
                    let groups = fact
                        .groups
                        .iter()
                        .filter(|(_, group)| roots.contains(&InventoryNodeKey::Group(**group)))
                        .map(|(ordinal, group)| (*ordinal, *group))
                        .collect();
                    (
                        owner.clone(),
                        RecordedInputNativeDecision {
                            role: fact.role.clone(),
                            groups,
                        },
                    )
                })
                .collect(),
        }
    }
}

/// Exact binding observations retained from successful compiler-role admission.
/// These entries are acquisition owners, not a reconstruction from payload IDs.
#[derive(Clone, Debug, Eq, PartialEq)]
struct ResolvedCompilerRole {
    interface: (ArtifactBinding, Arc<ArtifactEntry>),
    original: Option<(ArtifactBinding, Arc<ArtifactEntry>)>,
}

impl ResolvedCompilerRole {
    fn retain_acquisitions(&mut self, retained: &ArtifactView) -> Result<(), CompileError> {
        for (binding, entry) in std::iter::once(&mut self.interface).chain(self.original.iter_mut())
        {
            *entry = Arc::clone(
                retained
                    .lease
                    .payloads
                    .get(&binding.artifact)
                    .ok_or_else(|| failure("retained compiler role lost its payload owner"))?,
            );
        }
        Ok(())
    }
}

/// Borrowed selected interface observations and the same issuing view that
/// resolves their scoped bindings. Historical custody cannot choose a row.
pub(crate) struct SelectedCompilerInterfaces<'a> {
    artifacts: &'a ArtifactView,
    entries: BTreeMap<ExactModuleIdentity, Arc<ArtifactEntry>>,
}

impl SelectedCompilerInterfaces<'_> {
    pub(crate) fn entries(&self) -> &BTreeMap<ExactModuleIdentity, Arc<ArtifactEntry>> {
        &self.entries
    }

    pub(crate) fn support_for_owners(
        &self,
        owners: Vec<ExactModuleIdentity>,
    ) -> Result<CompilerSourceSupport, CompileError> {
        let mut projection = CompilerInputProjection::default();
        let mut pending = owners;
        while let Some(owner) = pending.pop() {
            if projection.roles.contains_key(&owner) {
                continue;
            }
            let entry = self
                .entries
                .get(&owner)
                .ok_or_else(|| failure("selected interface dependency lacks its issued role"))?;
            pending.extend(entry.requirements.iter().cloned());
            projection.admit_role(
                owner,
                CompilerInputRole::InterfaceOnly {
                    interface: entry.descriptor.id,
                },
            )?;
        }
        let artifacts = self.select_owners(projection.roles.keys().cloned().collect())?;
        CompilerSourceSupport::new(projection, artifacts, [])
    }

    pub(crate) fn select_owners(
        &self,
        owners: Vec<ExactModuleIdentity>,
    ) -> Result<ArtifactView, CompileError> {
        let ids = owners
            .iter()
            .map(|owner| {
                self.entries
                    .get(owner)
                    .map(|entry| entry.descriptor.id)
                    .ok_or_else(|| failure("selected interface owner lacks its issued role"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let state = self
            .artifacts
            .lease
            .inventory
            .0
            .lock()
            .expect("inventory lock");
        let owned = &self.artifacts.read_projection(&state).nodes;
        let roots = ids
            .iter()
            .map(|id| {
                self.artifacts
                    .resolve_binding(*id, owned)
                    .map(InventoryNodeKey::Artifact)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let interfaces = admitted_closure(&state, roots.into_iter());
        let namespace = artifact_ids(&interfaces)
            .into_iter()
            .map(|id| {
                self.artifacts
                    .resolve_binding(id, &interfaces)
                    .map(|binding| (id, binding))
            })
            .collect::<Result<BTreeMap<_, _>, _>>()?;
        drop(state);
        let mut selected = self.artifacts.select_roots(ids)?;
        selected.replace_namespace(&namespace);
        Ok(selected)
    }
}

impl CompilerSourceSupport {
    fn new(
        projection: CompilerInputProjection,
        artifacts: ArtifactView,
        native_offers: impl IntoIterator<Item = IssuedNativeOffer>,
    ) -> Result<Self, CompileError> {
        projection.validate(&artifacts)?;
        let mut selected = projection.resolve_associations(&artifacts)?;
        let mut offers = Vec::new();
        let retained = artifacts.retain_issued_offer_custody(
            native_offers.into_iter().filter_map(|offer| {
                if !offers.contains(&offer) {
                    offers.push(offer.clone());
                    Some(offer)
                } else {
                    None
                }
            }),
        )?;
        // Custody-only retention preserves these exact selected bindings while
        // it may acquire additional immutable sources for their payloads.
        for row in selected.values_mut() {
            row.retain_acquisitions(&retained)?;
        }
        Ok(Self {
            projection,
            artifacts: retained,
            native_offers: offers.into(),
            selected: Arc::new(selected),
            input_decisions: IssuedInputNativeDecisions::default(),
        })
    }

    fn rebuild(
        &self,
        projection: CompilerInputProjection,
        artifacts: ArtifactView,
        offers: impl IntoIterator<Item = IssuedNativeOffer>,
    ) -> Result<Self, CompileError> {
        let decisions = self.input_decisions.for_exact_roles(&projection);
        let mut support = Self::new(projection, artifacts, offers)?;
        support.input_decisions = decisions;
        Ok(support)
    }

    fn rebuild_narrowed(
        &self,
        projection: CompilerInputProjection,
        artifacts: ArtifactView,
        offers: impl IntoIterator<Item = IssuedNativeOffer>,
    ) -> Result<Self, CompileError> {
        let decisions = self.input_decisions.restricted(&projection, &artifacts);
        let mut support = Self::new(projection, artifacts, offers)?;
        support.input_decisions = decisions;
        Ok(support)
    }

    fn with_additional_decisions(
        mut self,
        incoming: Option<&IssuedInputNativeDecisions>,
    ) -> Result<Self, CompileError> {
        if let Some(incoming) = incoming {
            for (owner, fact) in incoming.for_exact_roles(&self.projection).facts {
                if let Some(previous) = self.input_decisions.facts.get_mut(&owner) {
                    for (ordinal, group) in fact.groups {
                        if previous
                            .groups
                            .insert(ordinal, group)
                            .is_some_and(|old| old != group)
                        {
                            return Err(admission_failure(
                                ArtifactInventoryFailure::BindingAmbiguity {
                                    artifact: group.binding.artifact(),
                                },
                            ));
                        }
                    }
                } else {
                    self.input_decisions.facts.insert(owner, fact);
                }
            }
        }
        Ok(self)
    }

    /// Issue availability at a checked original input ingress from its exact
    /// selected graph. Custody outside that selection cannot supply bodies.
    fn from_selected_issuance(
        projection: CompilerInputProjection,
        artifacts: ArtifactView,
    ) -> Result<Self, CompileError> {
        let mut offers = Vec::new();
        let mut input_decisions = IssuedInputNativeDecisions::default();
        for role in projection.roles() {
            if let Some(original) = role.original() {
                let proof = NativeOriginalRequirement::from_role(role.clone())
                    .map(|proof| Ok(Some(proof)))
                    .unwrap_or_else(|| {
                        NativeOriginalRequirement::from_selected_view(original, &artifacts)
                    })?;
                let groups = checked_input_group_choices(
                    original,
                    proof.iter().flat_map(|proof| {
                        proof
                            .selection
                            .snapshot()
                            .roots
                            .keys()
                            .filter_map(|node| match node {
                                ArtifactBindingNode::Group(group)
                                    if group.binding.artifact() == original =>
                                {
                                    Some(*group)
                                }
                                _ => None,
                            })
                            .collect::<Vec<_>>()
                    }),
                )?;
                let owner = artifacts
                    .lease
                    .payloads
                    .get(&original)
                    .ok_or_else(|| failure("checked original issuer lost its payload"))?
                    .descriptor
                    .owner
                    .clone();
                input_decisions.facts.insert(
                    owner,
                    RecordedInputNativeDecision {
                        role: role.clone(),
                        groups,
                    },
                );
                offers.push(IssuedNativeOffer::issue(original, &artifacts, proof)?);
            }
        }
        Self::new(projection, artifacts, offers).map(|mut support| {
            support.input_decisions = input_decisions;
            support
        })
    }

    /// Observe issued canonical interfaces while preserving compiler roles and
    /// native authority in the original support.
    pub(crate) fn selected_interfaces(
        &self,
    ) -> Result<SelectedCompilerInterfaces<'_>, CompileError> {
        let entries = self
            .selected
            .iter()
            .map(|(owner, row)| (owner.clone(), Arc::clone(&row.interface.1)))
            .collect();
        Ok(SelectedCompilerInterfaces {
            artifacts: &self.artifacts,
            entries,
        })
    }

    pub(crate) fn native_offer_observations(&self) -> Result<BTreeSet<[u8; 32]>, CompileError> {
        self.native_offers
            .iter()
            .map(|offer| offer.snapshot_facts().identity())
            .collect()
    }

    pub(crate) fn checked_native_input_decision(
        &self,
        original: ArtifactId,
        canonical: ArtifactId,
        native_owner: &tidepool_repr::execution_schema::CachedHomeOwner,
    ) -> Result<InputNativeDecision<'_>, crate::certified_products::InputDecisionRefusal> {
        use crate::certified_products::InputDecisionRefusal;
        let owner = ExactModuleIdentity {
            unit: native_owner.unit.clone(),
            module: native_owner.module.clone(),
        };
        let Some(role) = self.projection.roles.get(&owner) else {
            return Ok(InputNativeDecision::UnissuedPolicy);
        };
        let Some(selected_original) = role.original() else {
            return Ok(InputNativeDecision::UnissuedPolicy);
        };
        let admitted = self
            .selected
            .get(&owner)
            .ok_or(InputDecisionRefusal::ArtifactIdentityMismatch)?;
        let (_, entry) = admitted
            .original
            .as_ref()
            .ok_or(InputDecisionRefusal::ArtifactIdentityMismatch)?;
        if selected_original != original
            || entry.descriptor.id != original
            || role.interface() != canonical
            || admitted.interface.1.descriptor.id != canonical
        {
            return Err(InputDecisionRefusal::ArtifactIdentityMismatch);
        }
        let ArtifactPayload::Original(product) = &entry.payload else {
            return Err(InputDecisionRefusal::ArtifactIdentityMismatch);
        };
        if product.owner() != native_owner {
            return Err(InputDecisionRefusal::NativeOwnerMismatch);
        }
        match self.input_decisions.for_role(&owner, role) {
            InputNativeDecision::UnissuedPolicy => Err(InputDecisionRefusal::MissingDecision),
            issued => Ok(issued),
        }
    }

    pub(crate) fn input_decisions(&self) -> &IssuedInputNativeDecisions {
        &self.input_decisions
    }

    pub(crate) fn recorded_input_decisions(
        &self,
    ) -> &imbl::OrdMap<ExactModuleIdentity, RecordedInputNativeDecision> {
        &self.input_decisions.facts
    }

    pub(crate) fn native_offers(&self) -> &[IssuedNativeOffer] {
        &self.native_offers
    }

    pub(crate) fn close_request(
        &self,
        fragment: Option<&crate::declaration_context::OriginalCompilerInputs>,
        protected: &crate::declaration_context::ProtectedScaffoldRequirements,
    ) -> Result<Self, CompileError> {
        let (artifacts, projection, decisions) = match fragment {
            Some(fragment) => ArtifactView::receive_original_fragment(self, fragment)?,
            None => (
                self.artifacts.clone(),
                self.projection.clone(),
                self.input_decisions.clone(),
            ),
        };
        let projection = projection.merge(protected.role_requirements())?;
        let offers = self
            .native_offers
            .iter()
            .chain(
                fragment
                    .into_iter()
                    .flat_map(|fragment| fragment.issued_offers().iter()),
            )
            .cloned();
        // Availability is retained evidence. The request transport below owns
        // which exact own roots or dependency subclosures it consumes.
        let mut support = Self::new(projection, artifacts, offers)?;
        support.input_decisions = decisions;
        let has_unissued_original =
            support
                .projection
                .snapshot_roles()
                .iter()
                .any(|(owner, role)| {
                    role.original().is_some()
                        && support
                            .input_decisions
                            .facts
                            .get(owner)
                            .is_none_or(|fact| &fact.role != role)
                });
        if has_unissued_original {
            // Resolve policy only at the real input issuer. A carried empty
            // decision is already resolved and must suppress policy fallback.
            let (artifacts, projection, decisions) =
                support.artifacts.merge_compiler_inputs_with_demand(
                    &support.projection,
                    &support.artifacts,
                    &CompilerInputProjection::default(),
                    CompilerNativeDemand::SourceSelections {
                        left: Some(&support.input_decisions),
                        right: None,
                    },
                    CompilerInputValidation::ReceivedFragment,
                )?;
            let mut resolved =
                Self::new(projection, artifacts, support.native_offers.iter().cloned())?;
            resolved.input_decisions = decisions;
            return Ok(resolved);
        }
        Ok(support)
    }

    pub(crate) fn from_recovered_selection(
        selection: crate::declaration_context::RecoveredCompilerSelection,
    ) -> Result<Self, CompileError> {
        let (artifacts, projection, issuance) = selection.into_parts();
        match issuance {
            crate::declaration_context::RecoveredCompilerIssuance::CheckedInput => {
                Self::from_selected_issuance(projection, artifacts)
            }
            crate::declaration_context::RecoveredCompilerIssuance::RecordedRoles => {
                Self::new(projection, artifacts, [])
            }
        }
    }

    pub(crate) fn restore_snapshot_evidence(
        &self,
        snapshot: &crate::exact_context::ExactContextSnapshot,
    ) -> Result<Self, CompileError> {
        if self.projection.roles() != snapshot.compiler_roles() {
            return Err(failure(
                "recovered availability roles differ from their context",
            ));
        }
        let offers = snapshot
            .native_offers()
            .map(|fact| fact.restore(&self.artifacts))
            .collect::<Result<Vec<_>, _>>()?;
        let mut restored = self.rebuild(self.projection.clone(), self.artifacts.clone(), offers)?;
        let state = restored
            .artifacts
            .lease
            .inventory
            .0
            .lock()
            .expect("inventory lock");
        for (owner, fact) in &snapshot.input_decisions {
            if restored.projection.snapshot_roles().get(owner) != Some(&fact.role) {
                return Err(failure(
                    "recorded input decision differs from its exact compiler role",
                ));
            }
            let original = fact
                .role
                .original()
                .ok_or_else(|| failure("recorded input decision has no original"))?;
            for (ordinal, group) in &fact.groups {
                let node = InventoryNodeKey::Group(*group);
                if *ordinal != group.original_ordinal
                    || group.binding.artifact() != original
                    || !(restored.artifacts.roots().contains(&node)
                        || restored
                            .native_offers
                            .iter()
                            .any(|offer| offer.contains_checked_group(*group)))
                    || !state.contains_node(&node)
                {
                    return Err(failure(
                        "recorded input decision is outside its authenticated custody",
                    ));
                }
                let carriers = state
                    .outgoing(node)
                    .filter_map(|(target, dependency)| match target {
                        InventoryNodeKey::Artifact(carrier)
                            if *dependency == ArtifactDependency::Interface
                                && carrier.artifact == original =>
                        {
                            Some(carrier)
                        }
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                let [carrier] = carriers.as_slice() else {
                    return Err(failure(
                        "recorded input decision has no unique original carrier",
                    ));
                };
                if !restored
                    .artifacts
                    .lease
                    .selection
                    .scopes
                    .contains_key(&carrier.selection)
                    || !restored.artifacts.lease.payloads.contains_key(&original)
                {
                    return Err(failure(
                        "recorded input decision lost its acquisition owner",
                    ));
                }
            }
        }
        drop(state);
        restored.input_decisions = IssuedInputNativeDecisions {
            facts: snapshot.input_decisions.clone(),
        };
        Ok(restored)
    }

    pub(crate) fn from_joined_interface(
        interface: &crate::recovery_artifacts::CertifiedJoinedInterface,
        inputs: &crate::declaration_join::ValidatedJoinInputs,
    ) -> Result<Self, CompileError> {
        let source = inputs.support();
        let entry = Arc::new(ArtifactEntry::interface(
            interface.clone(),
            JoinedInterfaceRole::LexicalJoin,
            inputs.owners().to_vec(),
        ));
        let issued = CompilerInputProjection::from_issued_entries(&[entry.clone()])?;
        let namespace = source.projection.merge(&issued)?;
        let joined = source
            .artifacts
            .inventory()
            .admit_projected_with_native_requirements(
                &source.artifacts,
                vec![entry],
                &namespace.interface_only(),
                &[],
            )?;
        let joined = joined.merge_selected(&source.artifacts, &namespace)?;
        source.rebuild(namespace, joined, source.native_offers.iter().cloned())
    }

    pub(crate) fn close_complete_output(
        output: &crate::declaration_context::OriginalCompilerInputs,
    ) -> Result<Self, CompileError> {
        Self::new(
            output.role_projection().clone(),
            output.issued_artifacts().clone(),
            output.issued_offers().iter().cloned(),
        )
        .and_then(|support| support.with_additional_decisions(output.issued_input_decisions()))
    }

    pub(crate) fn from_original_output(
        request: &crate::declaration_context::ExactCompilationRequest,
        output: &crate::declaration_context::OriginalCompilerInputs,
    ) -> Result<Self, CompileError> {
        let projection = request
            .compiler_inputs()
            .projection()
            .within_view(output.issued_artifacts())
            .merge(output.role_projection())?;
        request
            .compiler_inputs()
            .compiler_source_support()
            .rebuild(
                projection,
                output.issued_artifacts().clone(),
                request
                    .context()
                    .compiler_source_support()
                    .native_offers()
                    .iter()
                    .chain(output.issued_offers())
                    .cloned(),
            )
            .and_then(|support| support.with_additional_decisions(output.issued_input_decisions()))
    }

    pub(crate) fn extend_program_source(
        &self,
        selection: &crate::certified_products::CertifiedSourceSelection,
        artifacts: &ArtifactView,
        admissions: &[crate::declaration_context::ExactSourceAdmission],
    ) -> Result<Self, CompileError> {
        let mut owners = BTreeSet::new();
        for admission in admissions {
            owners.extend(admission.home_imports()?.into_keys());
        }
        let incoming = selection
            .compiler_support_projection(artifacts, admissions)
            .map_err(|error| CompileError::CompilerEvidence(Box::new(error)))?
            .for_source_owners(&owners);
        let continuation =
            IssuedInputNativeDecisions::from_certified_continuation(selection, &incoming)?;
        let (artifacts, projection, decisions) = self.artifacts.merge_compiler_inputs_with_demand(
            &self.projection,
            artifacts,
            &incoming,
            CompilerNativeDemand::SourceSelections {
                left: Some(&self.input_decisions),
                right: Some(&continuation),
            },
            CompilerInputValidation::ReceivedFragment,
        )?;
        let offers = selection
            .issued_native_availability()
            .flat_map(|availability| availability.offers().iter().cloned());
        self.rebuild(
            projection,
            artifacts,
            self.native_offers.iter().cloned().chain(offers),
        )
        .map(|mut support| {
            support.input_decisions = decisions;
            support
        })
    }

    pub(crate) fn empty() -> Self {
        Self::new(
            CompilerInputProjection::default(),
            ArtifactInventory::default().empty_view(),
            [],
        )
        .expect("empty compiler support is closed")
    }

    pub(crate) fn retain_custody(&self, incoming: &ArtifactView) -> Result<Self, CompileError> {
        self.rebuild(
            self.projection.clone(),
            self.artifacts.retain_custody(incoming)?,
            self.native_offers.iter().cloned(),
        )
    }

    pub(crate) fn retain_offers(
        &self,
        incoming: impl IntoIterator<Item = IssuedNativeOffer>,
    ) -> Result<Self, CompileError> {
        let incoming = incoming.into_iter().collect::<Vec<_>>();
        if incoming.iter().all(|incoming| {
            self.native_offers
                .iter()
                .any(|retained| retained.same_issuance(incoming))
        }) {
            // These exact issuances already entered this closed owner in its
            // retained order. No new acquisition or seed is being attached.
            return Ok(self.clone());
        }
        self.rebuild(
            self.projection.clone(),
            self.artifacts.clone(),
            self.native_offers.iter().cloned().chain(incoming),
        )
    }

    pub(crate) fn bind_snapshot_baseline(
        &self,
        snapshot: &crate::exact_context::ExactContextSnapshot,
    ) -> Result<Self, CompileError> {
        let mut projection = self.projection.clone();
        projection.bind_snapshot_roles(&snapshot.roles)?;
        let mut artifacts = self.artifacts.clone();
        artifacts.bind_selection_snapshot_baseline(snapshot.selection().clone())?;
        self.rebuild(projection, artifacts, self.native_offers.iter().cloned())
    }

    pub(crate) fn finalize_namespace(&self) -> Result<Self, CompileError> {
        self.rebuild(
            self.projection.clone(),
            self.artifacts.with_compiler_projection(&self.projection)?,
            self.native_offers.iter().cloned(),
        )
    }

    pub(crate) fn for_source_owners(
        &self,
        owners: &BTreeSet<ExactModuleIdentity>,
    ) -> Result<Self, CompileError> {
        let projection = self.projection.for_source_owners(owners);
        if projection == self.projection {
            return Ok(self.clone());
        }
        let mut entries = BTreeMap::new();
        for row in self.selected.values() {
            for (_, entry) in std::iter::once(&row.interface).chain(row.original.iter()) {
                entries.insert(entry.descriptor.id, Arc::clone(entry));
            }
        }
        // This operation changes policy over existing selected bindings only.
        // Canonical requirements may differ from an original's requirements,
        // so validate the resulting actual dependency/seal relationships.
        projection.entries_from_artifacts(&entries)?;
        let mut selected = self.selected.as_ref().clone();
        for (owner, role) in &projection.roles {
            let row = selected.get_mut(owner).expect("existing compiler role");
            if row.interface.0.artifact != role.interface() {
                return Err(failure(
                    "source policy changed its admitted canonical binding",
                ));
            }
            match (role.original(), row.original.as_ref()) {
                (None, _) => row.original = None,
                (Some(id), Some((binding, _))) if id == binding.artifact => {}
                _ => {
                    return Err(failure(
                        "source policy changed its admitted original binding",
                    ))
                }
            }
        }
        let input_decisions = self.input_decisions.for_exact_roles(&projection);
        Ok(Self {
            projection,
            artifacts: self.artifacts.clone(),
            native_offers: Arc::clone(&self.native_offers),
            selected: Arc::new(selected),
            input_decisions,
        })
    }

    pub(crate) fn certify_original_closure(
        &self,
        originals: &crate::certified_products::SelectedOriginalClosure,
    ) -> Result<Self, CompileError> {
        let (artifacts, projection, decisions) = self.artifacts.merge_selected_original_closure(
            &self.projection,
            originals,
            &self.input_decisions,
        )?;
        self.rebuild(projection, artifacts, self.native_offers.iter().cloned())
            .map(|mut support| {
                support.input_decisions = decisions;
                support
            })
    }

    #[cfg(test)]
    pub(crate) fn admit_checked_products(
        &self,
        producer: [u8; 32],
        products: &[CertifiedRecoveryProduct],
    ) -> Result<Self, CompileError> {
        if products.is_empty() {
            return Ok(self.clone());
        }
        let fragment =
            crate::declaration_context::OriginalCompilerInputs::from_checked_support_products(
                self, producer, products,
            )?;
        let (artifacts, projection, decisions) =
            ArtifactView::receive_original_fragment(self, &fragment)?;
        self.rebuild(
            projection,
            artifacts,
            self.native_offers
                .iter()
                .chain(fragment.issued_offers())
                .cloned(),
        )
        .map(|mut support| {
            support.input_decisions = decisions;
            support
        })
    }

    pub(crate) fn admit_value_interfaces(
        &self,
        values: &[Arc<crate::recovery_artifacts::CertifiedValueInterface>],
    ) -> Result<Self, CompileError> {
        let entries = values
            .iter()
            .map(|value| {
                Arc::new(ArtifactEntry::interface(
                    value.interface().clone(),
                    JoinedInterfaceRole::ValueInterface,
                    value.requirements().to_vec(),
                ))
            })
            .collect::<Vec<_>>();
        let issued = CompilerInputProjection::from_issued_entries(&entries)?;
        let projection = self.projection.merge(&issued)?;
        let catalog = NativeDependencyCatalog::from_source(self)?;
        let artifacts = projection.admit_interfaces_with_native_offers(
            &self.artifacts,
            entries,
            &self.native_offers,
            &catalog,
        )?;
        self.rebuild(projection, artifacts, self.native_offers.iter().cloned())
    }

    pub(crate) fn without_value_roots(
        &self,
        excluded: &BTreeSet<ArtifactId>,
    ) -> Result<Self, CompileError> {
        let artifacts = self.artifacts.without_value_interface_roots(excluded)?;
        let projection = self.projection.within_view(&artifacts);
        self.rebuild(projection, artifacts, self.native_offers.iter().cloned())
    }

    /// The freshly checked value certificate selects its exact canonical role;
    /// retained artifact IDs are not an input to this boundary.
    pub(crate) fn select_value_interface(
        &self,
        certificate: &crate::recovery_artifacts::CertifiedValueInterface,
    ) -> Result<Self, CompileError> {
        let selected = self.selected_interfaces()?;
        let owner = selected
            .entries()
            .iter()
            .find(|(_, entry)| entry.descriptor.id == certificate.artifact_id())
            .map(|(owner, _)| owner.clone())
            .ok_or_else(|| failure("value certificate is outside selected compiler interfaces"))?;
        selected.support_for_owners(vec![owner])
    }

    pub(crate) fn select_publication_graph(
        &self,
        root: &ExactModuleIdentity,
        roots: Vec<ArtifactId>,
        owners: &BTreeSet<ExactModuleIdentity>,
    ) -> Result<Self, CompileError> {
        let role = self
            .projection
            .snapshot_roles()
            .get(root)
            .ok_or_else(|| failure("published root lacks its issued role"))?;
        let original = role
            .original()
            .ok_or_else(|| failure("published root lacks an original"))?;
        let artifacts = match role {
            CompilerInputRole::PublishedSourceOriginal { selection, .. } => {
                self.artifacts.select_issued(roots, selection)?
            }
            _ => self.artifacts.select_publication_roots(original, roots)?,
        };
        let ids = artifacts
            .artifact_ids()
            .into_iter()
            .collect::<BTreeSet<_>>();
        let projection = self.projection.project_roles(|owner, role| {
            (owners.contains(owner) && ids.contains(&role.interface())).then(|| {
                if owner == root && role.is_published_source_original() {
                    role.clone()
                } else if let Some(original) = role.original().filter(|id| ids.contains(id)) {
                    CompilerInputRole::ReusableOriginal {
                        interface: role.interface(),
                        original,
                    }
                } else {
                    CompilerInputRole::InterfaceOnly {
                        interface: role.interface(),
                    }
                }
            })
        });
        self.rebuild_narrowed(projection, artifacts, self.native_offers.iter().cloned())
    }

    pub(crate) fn publish_original(
        &self,
        endorsement: &crate::declaration_context::PublishedOriginalEndorsement,
    ) -> Result<Self, CompileError> {
        let role = endorsement.role();
        let previous = self
            .projection
            .snapshot_roles()
            .get(endorsement.owner())
            .ok_or_else(|| failure("publication lacks its issued compiler role"))?;
        let CompilerInputRole::PublishedSourceOriginal { selection, .. } = role else {
            return Err(failure("publication endorsement lacks published policy"));
        };
        if previous.interface() != role.interface()
            || previous.original() != role.original()
            || selection.graph_selection() != &self.artifacts.capture_graph_selection()
        {
            return Err(failure(
                "publication endorsement differs from its exact issuer",
            ));
        }
        let carried = self
            .input_decisions
            .facts
            .get(endorsement.owner())
            .filter(|fact| &fact.role == previous)
            .cloned();
        let mut projection = self.projection.clone();
        projection.admit_role(endorsement.owner().clone(), role.clone())?;
        self.rebuild(
            projection,
            self.artifacts.clone(),
            self.native_offers.iter().cloned(),
        )
        .map(|mut support| {
            if let Some(mut fact) = carried {
                // The endorsement above proves this specific role transition;
                // generic role replacement cannot carry another role's choice.
                fact.role = role.clone();
                support
                    .input_decisions
                    .facts
                    .insert(endorsement.owner().clone(), fact);
            }
            support
        })
    }

    pub(crate) fn select_support_roots(
        &self,
        roots: Vec<ArtifactId>,
    ) -> Result<Self, CompileError> {
        let artifacts = self.artifacts.select_roots(roots)?;
        let projection = self.projection.for_selected_support(&artifacts);
        let offers = self
            .native_offers
            .iter()
            .filter_map(|offer| match offer.is_relevant_to_support(&projection) {
                Ok(true) => Some(Ok(offer.clone())),
                Ok(false) => None,
                Err(error) => Some(Err(error)),
            })
            .collect::<Result<Vec<_>, _>>()?;
        self.rebuild_narrowed(projection, artifacts, offers)
    }

    pub(crate) fn source_support(
        &self,
        owners: &BTreeSet<ExactModuleIdentity>,
    ) -> Result<Self, CompileError> {
        let selected = self.projection.source_support(&self.artifacts, owners)?;
        let offers = self
            .native_offers
            .iter()
            .filter_map(
                |offer| match offer.is_relevant_to_support(&selected.projection) {
                    Ok(true) => Some(Ok(offer.clone())),
                    Ok(false) => None,
                    Err(error) => Some(Err(error)),
                },
            )
            .collect::<Result<Vec<_>, _>>()?;
        self.rebuild_narrowed(selected.projection, selected.artifacts, offers)
    }

    /// Type evidence adds selected interfaces while the receiver continues to
    /// own its exact execution demand independently of published source policy.
    pub(crate) fn extend_interfaces(
        &self,
        interfaces: &SelectedCompilerInterfaces<'_>,
    ) -> Result<Self, CompileError> {
        let (artifacts, projection, _) = self
            .artifacts
            .merge_compiler_interfaces(&self.projection, interfaces)?;
        self.rebuild(projection, artifacts, self.native_offers.iter().cloned())
    }

    pub(crate) fn merge(&self, other: &Self) -> Result<Self, CompileError> {
        let (artifacts, projection, decisions) = self.artifacts.merge_compiler_inputs_with_demand(
            &self.projection,
            &other.artifacts,
            &other.projection,
            CompilerNativeDemand::SourceSelections {
                left: Some(&self.input_decisions),
                right: Some(&other.input_decisions),
            },
            CompilerInputValidation::Closed,
        )?;
        self.rebuild(
            projection,
            artifacts,
            self.native_offers
                .iter()
                .chain(other.native_offers.iter())
                .cloned(),
        )
        .map(|mut support| {
            support.input_decisions = decisions;
            support
        })
    }

    /// Lifetime custody includes authored private helpers, independently of
    /// current executable demand and without choosing a historical issuer.
    pub(crate) fn authored_native_binding_custody_requirements(
        &self,
        accepted: &BTreeSet<ExactModuleIdentity>,
    ) -> Result<Vec<NativeBindingRequirement>, CompileError> {
        let mut requirements = self
            .artifacts
            .authored_native_binding_custody_requirements(accepted)?
            .into_iter()
            .collect::<BTreeSet<_>>();
        for offer in self.native_offers.iter() {
            requirements.extend(offer.authored_binding_requirements(accepted)?);
        }
        Ok(requirements.into_iter().collect())
    }

    pub(crate) fn projection(&self) -> &CompilerInputProjection {
        &self.projection
    }

    pub(crate) fn artifacts(&self) -> &ArtifactView {
        &self.artifacts
    }
}

/// Only the projection owner can classify offers as namespace contributions.
/// Private-qualified body custody is deliberately absent from this view.
pub(super) struct NativeNamespaceContributions(Vec<IssuedNativeOffer>);

impl NativeNamespaceContributions {
    pub(super) fn offers(&self) -> &[IssuedNativeOffer] {
        &self.0
    }
}

impl CompilerInputProjection {
    /// Retained offers keep their original exact custody. Current admissions
    /// consume only contributions derived from this issued compiler namespace.
    fn native_namespace_contributions(
        &self,
        parent: &ArtifactView,
        offers: &[IssuedNativeOffer],
    ) -> Result<NativeNamespaceContributions, CompileError> {
        let originals = self
            .roles()
            .into_iter()
            .filter_map(|role| role.original())
            .collect::<BTreeSet<_>>();
        let mut contributions = Vec::new();
        for offer in offers {
            let Some(consumed) = offer.within_projection(self, parent)? else {
                continue;
            };
            if originals.contains(&consumed.original()) || consumed.offers_groups_for(&originals) {
                contributions.push(consumed);
            } else if consumed
                .qualified_private_availability(self, parent)
                .is_none()
            {
                return Err(admission_failure(
                    ArtifactInventoryFailure::NativeRootOutsideView {
                        artifact: consumed.original(),
                    },
                ));
            }
        }
        Ok(NativeNamespaceContributions(contributions))
    }

    pub(crate) fn admit_interfaces_with_native_offers(
        &self,
        parent: &ArtifactView,
        entries: Vec<Arc<ArtifactEntry>>,
        offers: &[IssuedNativeOffer],
        catalog: &NativeDependencyCatalog<'_>,
    ) -> Result<ArtifactView, CompileError> {
        let contributions = self.native_namespace_contributions(parent, offers)?;
        parent.inventory().admit_projected_with_native_offers(
            parent,
            entries,
            self,
            &contributions,
            Some(catalog),
        )
    }

    // Pure initial-policy oracle for explicitly issued original/canonical pairs.
    // Existing request/source continuations must use the catalog-bearing entry.
    #[cfg(test)]
    pub(crate) fn admit_interfaces_under_unissued_policy(
        &self,
        parent: &ArtifactView,
        entries: Vec<Arc<ArtifactEntry>>,
        offers: &[IssuedNativeOffer],
    ) -> Result<ArtifactView, CompileError> {
        let contributions = self.native_namespace_contributions(parent, offers)?;
        parent.inventory().admit_projected_with_native_offers(
            parent,
            entries,
            self,
            &contributions,
            None,
        )
    }

    pub(crate) fn issue_complete_native_availability(
        &self,
        parent: &ArtifactView,
        entries: Vec<Arc<ArtifactEntry>>,
        offers: &[IssuedNativeOffer],
        dependencies: &NativeDependencyCatalog<'_>,
    ) -> Result<super::native_availability::CheckedNativeAvailability, CompileError> {
        let contributions = self.native_namespace_contributions(parent, offers)?;
        parent.inventory().issue_private_complete_availability(
            parent,
            entries,
            self,
            &contributions,
            dependencies,
        )
    }

    /// Narrow one issued namespace to authenticated source owners and their
    /// exact required type closure. Reuse remains restricted to source owners;
    /// other dependencies keep interface roles and their existing graph only.
    pub(crate) fn source_support(
        &self,
        view: &ArtifactView,
        source_owners: &BTreeSet<ExactModuleIdentity>,
    ) -> Result<CompilerSourceSupport, CompileError> {
        let metadata = view.metadata_snapshot();
        let mut projection = Self::default();
        let mut pending = source_owners
            .iter()
            .filter(|owner| self.roles.contains_key(*owner))
            .cloned()
            .collect::<Vec<_>>();
        while let Some(owner) = pending.pop() {
            if projection.roles.contains_key(&owner) {
                continue;
            }
            let role = self
                .roles
                .get(&owner)
                .ok_or_else(|| failure("source compiler dependency lacks its issued role"))?;
            let role = if source_owners.contains(&owner) {
                role.clone()
            } else {
                CompilerInputRole::InterfaceOnly {
                    interface: role.interface(),
                }
            };
            let id = role.original().unwrap_or(role.interface());
            let entry = metadata
                .artifacts
                .get(&id)
                .ok_or_else(|| failure("source compiler role is outside its issued custody"))?;
            pending.extend(entry.requirements.iter().cloned());
            projection.admit_role(owner, role)?;
        }
        let artifacts = view.select_compiler_support(&projection)?;
        CompilerSourceSupport::new(projection, artifacts, [])
    }

    pub(crate) fn snapshot_roles(&self) -> &imbl::OrdMap<ExactModuleIdentity, CompilerInputRole> {
        &self.roles
    }
    pub(crate) fn bind_snapshot_roles(
        &mut self,
        roles: &imbl::OrdMap<ExactModuleIdentity, CompilerInputRole>,
    ) -> Result<(), CompileError> {
        if &self.roles != roles {
            return Err(failure(
                "restored compiler roles differ from exact snapshot",
            ));
        }
        self.roles = roles.clone();
        Ok(())
    }
    pub fn roles(&self) -> Vec<CompilerInputRole> {
        self.roles.values().cloned().collect()
    }

    pub(crate) fn semantic_observations(
        &self,
    ) -> Result<Vec<CompilerRoleObservation<'_>>, CompileError> {
        self.roles
            .values()
            .map(CompilerInputRole::semantic_observation)
            .collect()
    }

    fn project_roles(
        &self,
        mut select: impl FnMut(&ExactModuleIdentity, &CompilerInputRole) -> Option<CompilerInputRole>,
    ) -> Self {
        let mut roles = self.roles.clone();
        for (owner, previous) in &self.roles {
            match select(owner, previous) {
                Some(role) if &role == previous => {}
                Some(role) => {
                    roles.insert(owner.clone(), role);
                }
                None => {
                    roles.remove(owner);
                }
            }
        }
        Self { roles }
    }

    pub(crate) fn from_issued_entries(
        entries: &[Arc<ArtifactEntry>],
    ) -> Result<Self, CompileError> {
        let mut projection = Self::default();
        for entry in entries {
            let (interface, original_reuse) = match &entry.payload {
                ArtifactPayload::Original(product) => (
                    ArtifactEntry::canonical(
                        product
                            .module_interface()
                            .ok_or_else(|| failure("native compiler role lacks its interface"))?
                            .clone(),
                    )
                    .descriptor
                    .id,
                    Some(entry.descriptor.id),
                ),
                _ => (entry.descriptor.id, None),
            };
            let role = match original_reuse {
                Some(original) => CompilerInputRole::ReusableOriginal {
                    interface,
                    original,
                },
                None => CompilerInputRole::InterfaceOnly { interface },
            };
            projection.admit_role(entry.descriptor.owner.clone(), role)?;
        }
        Ok(projection)
    }

    /// Restrict already issued roles to a proved retained surface. An original
    /// absent from that surface loses its reuse offer, never its exact type role.
    /// Published selections retain their policy so missing custody refuses at
    /// validation instead of silently changing source-selection behavior.
    pub(crate) fn within_view(&self, view: &ArtifactView) -> Self {
        let ids = view
            .descriptors()
            .into_iter()
            .map(|row| row.id)
            .collect::<BTreeSet<_>>();
        self.project_roles(|_, role| {
            (ids.contains(&role.interface()) || role.is_published_source_original()).then(|| {
                match role.original() {
                    _ if role.is_published_source_original() => role.clone(),
                    Some(original) if ids.contains(&original) => role.clone(),
                    _ => CompilerInputRole::InterfaceOnly {
                        interface: role.interface(),
                    },
                }
            })
        })
    }

    /// A transient source or value support view owns only its selected carriers.
    /// Persistent published policy stays in the parent declaration namespace;
    /// a canonical dependency here does not offer its absent implementation.
    pub(crate) fn for_selected_support(&self, view: &ArtifactView) -> Self {
        let ids = view.artifact_ids().into_iter().collect::<BTreeSet<_>>();
        self.project_roles(|_, role| {
            ids.contains(&role.interface())
                .then(|| match role.original() {
                    Some(original) if ids.contains(&original) => role.clone(),
                    _ => CompilerInputRole::InterfaceOnly {
                        interface: role.interface(),
                    },
                })
        })
    }

    pub(crate) fn admit_role(
        &mut self,
        owner: ExactModuleIdentity,
        incoming: CompilerInputRole,
    ) -> Result<(), CompileError> {
        if self.roles.get(&owner) == Some(&incoming) {
            return Ok(());
        }
        if let Some(previous) = self.roles.get_mut(&owner) {
            if let (Some(existing), Some(new)) = (previous.original(), incoming.original()) {
                if existing != new {
                    return Err(admission_failure(
                        ArtifactInventoryFailure::CompilerOriginalOfferConflict {
                            owner,
                            existing,
                            incoming: new,
                        },
                    ));
                }
            }
            if previous.interface() != incoming.interface() {
                return Err(ArtifactInventoryError {
                    failure: ArtifactInventoryFailure::OwnerConflict { owner },
                    diagnostic_artifacts: None,
                    owner_conflict: Some(ArtifactOwnerConflictEvidence::CompilerRole {
                        existing: previous.clone(),
                        incoming,
                    }),
                }
                .into());
            }
            if previous.is_published_source_original()
                && incoming.is_published_source_original()
                && *previous != incoming
            {
                return Err(failure("conflicting published source original selection"));
            }
            match (previous.original(), incoming.original()) {
                (None, Some(_)) => *previous = incoming,
                _ if incoming.is_published_source_original() => *previous = incoming,
                _ => {}
            }
        } else {
            self.roles.insert(owner, incoming);
        }
        Ok(())
    }

    pub(crate) fn merge(&self, other: &Self) -> Result<Self, CompileError> {
        let mut merged = self.clone();
        for (owner, role) in &other.roles {
            merged.admit_role(owner.clone(), role.clone())?;
        }
        Ok(merged)
    }

    pub(crate) fn interface_only(&self) -> Self {
        self.project_roles(|_, role| {
            Some(CompilerInputRole::InterfaceOnly {
                interface: role.interface(),
            })
        })
    }

    /// Importing a proved source surface preserves exact interface custody.
    /// Only its issued source roles offer native originals in the new compiler
    /// namespace; unrelated executable dependencies remain retained originals.
    pub(crate) fn for_source_owners(&self, owners: &BTreeSet<ExactModuleIdentity>) -> Self {
        self.project_roles(|owner, role| {
            Some(
                if owners.contains(owner) || role.is_published_source_original() {
                    role.clone()
                } else {
                    CompilerInputRole::InterfaceOnly {
                        interface: role.interface(),
                    }
                },
            )
        })
    }

    pub(crate) fn project_metadata(
        &self,
        metadata: Arc<RetainedArtifactMetadata>,
    ) -> Result<CompilerArtifactMetadata, CompileError> {
        Ok(CompilerArtifactMetadata {
            entries: self.entries_from_metadata(&metadata)?,
            retained: metadata,
        })
    }

    /// Resolve selected type evidence by its issued interface ID, independently
    /// of whether the compiler role also offers an original implementation.
    pub(crate) fn selected_canonical_interfaces(
        &self,
        view: &ArtifactView,
    ) -> Result<
        BTreeMap<ExactModuleIdentity, crate::certified_products::CertifiedModuleInterface>,
        CompileError,
    > {
        let metadata = view.metadata_snapshot();
        self.entries_from_metadata(&metadata)?;
        Ok(self
            .roles
            .iter()
            .filter_map(
                |(owner, role)| match &metadata.artifacts[&role.interface()].payload {
                    ArtifactPayload::Canonical(interface) => {
                        Some((owner.clone(), interface.clone()))
                    }
                    _ => None,
                },
            )
            .collect())
    }

    /// Retained authored bodies can supplement selected type evidence privately.
    /// An original role restricts availability to its exact implementation; an
    /// interface-only role does not select executable groups or mutate its role.
    #[cfg(test)]
    pub(crate) fn native_declaration_availability(
        &self,
        view: &ArtifactView,
    ) -> Result<Vec<CertifiedRecoveryProduct>, CompileError> {
        let interfaces = self.selected_canonical_interfaces(view)?;
        Ok(view
            .entries()
            .into_iter()
            .filter_map(|entry| {
                let ArtifactPayload::Original(product) = &entry.payload else {
                    return None;
                };
                let role = self.roles.get(&entry.descriptor.owner)?;
                let interface = product.module_interface()?;
                if !matches!(
                    interface.origin(),
                    crate::certified_products::CanonicalOrigin::NativeAuthoredDeclaration { .. }
                ) || interfaces.get(&entry.descriptor.owner) != Some(interface)
                    || role.original().is_some_and(|id| id != entry.descriptor.id)
                {
                    return None;
                }
                Some(product.clone())
            })
            .collect())
    }

    /// Restoration consumes already authenticated original inventory, never a
    /// recovered owner name or an ordinal alone. Every role is checked before
    /// any selected compiler input is published.
    pub(crate) fn restore(
        view: &ArtifactView,
        roles: &[CompilerInputRole],
    ) -> Result<Self, CompileError> {
        let mut projection = Self::default();
        let mut seen = BTreeSet::new();
        let metadata = view.metadata_snapshot();
        for role in roles {
            let owner = metadata
                .artifacts
                .get(&role.interface())
                .ok_or_else(|| failure("recovered compiler role is outside retained custody"))?
                .descriptor
                .owner
                .clone();
            if !seen.insert(owner.clone()) {
                return Err(failure("duplicate recovered compiler input role"));
            }
            projection.admit_role(owner, role.clone())?;
        }
        projection.validate(view)?;
        Ok(projection)
    }

    pub(crate) fn validate(&self, view: &ArtifactView) -> Result<(), CompileError> {
        self.entries_from_metadata(&view.metadata_snapshot())
            .map(|_| ())
    }

    pub(crate) fn entries_from_metadata(
        &self,
        metadata: &RetainedArtifactMetadata,
    ) -> Result<BTreeMap<ExactModuleIdentity, Arc<ArtifactEntry>>, CompileError> {
        self.entries_from_artifacts(&metadata.artifacts)
    }

    pub(crate) fn entries_from_artifacts(
        &self,
        artifacts: &BTreeMap<ArtifactId, Arc<ArtifactEntry>>,
    ) -> Result<BTreeMap<ExactModuleIdentity, Arc<ArtifactEntry>>, CompileError> {
        let selected = self.associated_entries(artifacts)?;
        for entry in selected.values() {
            for required in &entry.requirements {
                let role = self.roles.get(required).ok_or_else(|| {
                    admission_failure(ArtifactInventoryFailure::MissingDependency {
                        artifact: entry.descriptor.id,
                        dependent: entry.descriptor.owner.clone(),
                        required: required.clone(),
                        dependency: ArtifactDependency::Interface,
                    })
                })?;
                if entry.interface_seals.get(required).is_some_and(|seal| {
                    artifacts[&role.interface()].descriptor.interface_sha256 != *seal
                }) {
                    return Err(admission_failure(
                        ArtifactInventoryFailure::InterfaceSealMismatch {
                            dependent: entry.descriptor.owner.clone(),
                            required: required.clone(),
                        },
                    ));
                }
            }
        }
        Ok(selected)
    }
    pub(crate) fn validate_associations(&self, view: &ArtifactView) -> Result<(), CompileError> {
        self.resolve_associations(view).map(|_| ())
    }

    fn resolve_associations(
        &self,
        view: &ArtifactView,
    ) -> Result<BTreeMap<ExactModuleIdentity, ResolvedCompilerRole>, CompileError> {
        let metadata = view.metadata_snapshot();
        let entries = self.associated_entries(&metadata.artifacts)?;
        let state = view.lease.inventory.0.lock().expect("inventory lock");
        let owned = &view.read_projection(&state).nodes;
        let mut selected = BTreeMap::new();
        for (owner, role) in &self.roles {
            let canonical = view.resolve_binding(role.interface(), owned)?;
            let original = if let Some(original) = role.original() {
                let carrier = view.resolve_binding(original, owned)?;
                if !state
                    .outgoing(InventoryNodeKey::Artifact(carrier))
                    .any(|(target, edge)| {
                        target == InventoryNodeKey::Artifact(canonical)
                            && *edge == ArtifactDependency::Interface
                    })
                {
                    return Err(failure(
                        "compiler fragment canonical pair differs from its issuing bindings",
                    ));
                }
                Some((carrier, Arc::clone(&entries[owner])))
            } else {
                None
            };
            selected.insert(
                owner.clone(),
                ResolvedCompilerRole {
                    interface: (
                        canonical,
                        Arc::clone(&metadata.artifacts[&role.interface()]),
                    ),
                    original,
                },
            );
        }
        Ok(selected)
    }

    fn associated_entries(
        &self,
        artifacts: &BTreeMap<ArtifactId, Arc<ArtifactEntry>>,
    ) -> Result<BTreeMap<ExactModuleIdentity, Arc<ArtifactEntry>>, CompileError> {
        let mut selected = BTreeMap::new();
        for (owner, role) in &self.roles {
            let interface = artifacts
                .get(&role.interface())
                .ok_or_else(|| failure("compiler interface role is outside retained custody"))?;
            if &interface.descriptor.owner != owner || interface.is_native() {
                return Err(failure("compiler interface role has another owner or kind"));
            }
            let entry = match role.original() {
                None => interface,
                Some(id) => {
                    let original = artifacts.get(&id).ok_or_else(|| {
                        failure("compiler original offer is outside retained custody")
                    })?;
                    let ArtifactPayload::Original(product) = &original.payload else {
                        return Err(failure("compiler original offer has no native proof"));
                    };
                    let canonical = product.module_interface().ok_or_else(|| {
                        failure("compiler original offer lacks canonical attachment")
                    })?;
                    if &original.descriptor.owner != owner
                        || ArtifactEntry::canonical(canonical.clone()).descriptor.id
                            != role.interface()
                    {
                        return Err(failure(
                            "compiler original offer differs from its interface",
                        ));
                    }
                    original
                }
            };
            selected.insert(owner.clone(), Arc::clone(entry));
        }
        Ok(selected)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolved_selected_reads_do_not_visit_historical_custody() {
        for group_count in [1, 4, 16] {
            let inventory = ArtifactInventory::default();
            let selected = Arc::new(crate::artifact_inventory::tests::entry(
                "Selected",
                &["Required"],
            ));
            let required = Arc::new(crate::artifact_inventory::tests::entry("Required", &[]));
            let expected = [selected.as_ref(), required.as_ref()]
                .into_iter()
                .map(|entry| {
                    let ArtifactPayload::Canonical(interface) = &entry.payload else {
                        panic!("canonical fixture")
                    };
                    (
                        entry.descriptor.owner.clone(),
                        interface.interface_bytes().to_vec(),
                    )
                })
                .collect::<BTreeMap<_, _>>();
            let product = crate::certified_products::fixture_finalized_product(
                crate::certified_products::tests::original_groups_fixture(
                    "Historical",
                    (0..group_count).map(|ordinal| (ordinal, vec![])).collect(),
                    1,
                    &BTreeMap::new(),
                ),
                [2; 32],
            );
            let original = crate::certified_products::tests::recovered_witness_fixtures(&[product])
                .remove(0)
                .product;
            let historical = Arc::new(ArtifactEntry::original([2; 32], original).unwrap());
            let view = inventory
                .admit_shared(
                    &inventory.empty_view(),
                    vec![
                        Arc::clone(&selected),
                        Arc::clone(&required),
                        Arc::clone(&historical),
                    ],
                )
                .unwrap();
            let projection = CompilerInputProjection::from_issued_entries(&[
                Arc::clone(&selected),
                Arc::clone(&required),
            ])
            .unwrap();
            let historical_projection =
                CompilerInputProjection::from_issued_entries(&[Arc::clone(&historical)]).unwrap();
            let offers =
                IssuedNativeOffer::issue_context_offers(&historical_projection, &view, &[])
                    .unwrap();
            assert!(!offers.is_empty());
            let support = CompilerSourceSupport::new(projection, view, offers).unwrap();
            assert!(
                support.artifacts.reads.read_projection.get().is_none(),
                "the real offer retention must leave a cold returned view for this control"
            );
            let before = inventory.metrics();
            let view_owner = Arc::clone(&support.artifacts.lease);
            for _ in 0..4 {
                let observation = support.selected_interfaces().unwrap();
                let actual = observation
                    .entries()
                    .iter()
                    .map(|(owner, entry)| {
                        let ArtifactPayload::Canonical(interface) = &entry.payload else {
                            panic!("canonical observation")
                        };
                        (owner.clone(), interface.interface_bytes().to_vec())
                    })
                    .collect::<BTreeMap<_, _>>();
                assert_eq!(actual, expected);
            }
            for _ in 0..4 {
                let repeated = support
                    .retain_offers(support.native_offers.iter().cloned())
                    .unwrap();
                let unchanged = repeated.for_source_owners(&BTreeSet::new()).unwrap();
                assert!(Arc::ptr_eq(&support.selected, &unchanged.selected));
                assert!(Arc::ptr_eq(
                    &support.native_offers,
                    &unchanged.native_offers
                ));
                assert!(Arc::ptr_eq(
                    &support.artifacts.lease,
                    &unchanged.artifacts.lease
                ));
            }
            let after = inventory.metrics();
            assert_eq!(after.graph_visits, before.graph_visits);
            assert_eq!(after.entry_handle_copies, before.entry_handle_copies);
            assert!(Arc::ptr_eq(&view_owner, &support.artifacts.lease));

            let interfaces = support.selected_interfaces().unwrap();
            let selected_support = interfaces
                .support_for_owners(vec![selected.descriptor.owner.clone()])
                .unwrap();
            assert_eq!(
                selected_support
                    .projection
                    .snapshot_roles()
                    .keys()
                    .cloned()
                    .collect::<BTreeSet<_>>(),
                expected.keys().cloned().collect()
            );
            assert!(selected_support
                .artifacts
                .selected_native_groups()
                .is_empty());

            let reusable = CompilerSourceSupport::new(
                historical_projection,
                support.artifacts.clone(),
                support.native_offers.iter().cloned(),
            )
            .unwrap();
            let original_graph = reusable.artifacts.capture_graph_selection();
            let before_policy = inventory.metrics();
            let interface_only = reusable.for_source_owners(&BTreeSet::new()).unwrap();
            assert!(interface_only
                .projection
                .roles()
                .iter()
                .all(|role| role.original().is_none()));
            assert_eq!(
                interface_only
                    .projection
                    .snapshot_roles()
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>(),
                vec![historical.descriptor.owner.clone()]
            );
            assert!(Arc::ptr_eq(
                &reusable.artifacts.lease,
                &interface_only.artifacts.lease
            ));
            assert!(Arc::ptr_eq(
                &reusable.native_offers,
                &interface_only.native_offers
            ));
            let after_policy = inventory.metrics();
            assert_eq!(after_policy.graph_visits, before_policy.graph_visits);
            assert_eq!(
                after_policy.entry_handle_copies,
                before_policy.entry_handle_copies
            );
            assert_eq!(
                interface_only.artifacts.capture_graph_selection(),
                original_graph
            );

            let missing =
                CompilerInputProjection::from_issued_entries(&[Arc::clone(&selected)]).unwrap();
            let refusal =
                CompilerSourceSupport::new(missing, support.artifacts.clone(), []).unwrap_err();
            assert!(matches!(refusal, CompileError::ArtifactInventory(error)
                if matches!(&error.failure, ArtifactInventoryFailure::MissingDependency { dependent, required: dependency, .. }
                    if dependent == &selected.descriptor.owner && dependency == &required.descriptor.owner)));
            assert_eq!(
                support.selected_interfaces().unwrap().entries().len(),
                expected.len()
            );
        }
    }

    #[test]
    fn checked_input_issuance_refuses_distinct_group_choices_before_recording_domain() {
        let product = crate::certified_products::fixture_finalized_product(
            crate::certified_products::tests::original_groups_fixture(
                "InputChoice",
                vec![(7, vec![])],
                1,
                &BTreeMap::new(),
            ),
            [2; 32],
        );
        let product = crate::certified_products::tests::recovered_witness_fixtures(&[product])
            .remove(0)
            .product;
        let original = Arc::new(ArtifactEntry::original([2; 32], product).unwrap());
        let inventory = ArtifactInventory::default();
        let sources = ["LeftMarker", "RightMarker"].map(|marker| {
            inventory
                .admit_shared(
                    &inventory.empty_view(),
                    vec![
                        Arc::clone(&original),
                        Arc::new(crate::artifact_inventory::tests::entry(marker, &[])),
                    ],
                )
                .unwrap()
        });
        let projection =
            CompilerInputProjection::from_issued_entries(&sources[0].entries()).unwrap();
        let expected = sources[0]
            .capture_graph_selection()
            .roots
            .into_iter()
            .filter_map(|node| match node {
                ArtifactBindingNode::Group(group)
                    if group.binding.artifact() == original.descriptor.id =>
                {
                    Some(group)
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(expected.len(), 1);
        let repeated = sources[0].merge_fixture_selections(&sources[0]).unwrap();
        let admitted =
            CompilerSourceSupport::from_selected_issuance(projection.clone(), repeated).unwrap();
        let actual = admitted.input_decisions.facts[&original.descriptor.owner]
            .groups
            .values()
            .copied()
            .collect::<Vec<_>>();
        assert_eq!(
            actual, expected,
            "repeated exact issuance preserves its chosen Group"
        );
        for (left, right) in [(&sources[0], &sources[1]), (&sources[1], &sources[0])] {
            let ambiguous = left.merge_fixture_selections(right).unwrap();
            let choices = ambiguous
                .capture_graph_selection()
                .roots
                .into_iter()
                .filter_map(|node| match node {
                    ArtifactBindingNode::Group(group)
                        if group.binding.artifact() == original.descriptor.id =>
                    {
                        Some(group)
                    }
                    _ => None,
                })
                .collect::<BTreeSet<_>>();
            assert_eq!(
                choices.len(),
                2,
                "both genuine distinct issuers reach the production ingress"
            );
            assert!(
                matches!(CompilerSourceSupport::from_selected_issuance(projection.clone(), ambiguous),
                Err(CompileError::ArtifactInventory(error)) if matches!(error.failure, ArtifactInventoryFailure::BindingAmbiguity { artifact } if artifact == original.descriptor.id))
            );
        }
    }
}
