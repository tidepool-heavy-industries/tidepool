//! Compiler input roles are issued independently of retained native custody.
//! Serialized roles become authority only after checking their exact originals.

use super::*;

/// Persisted input-role facts. The interface is exact type custody; an original
/// offer additionally grants reuse of one authenticated immutable implementation.
/// Constructing or decoding this record grants neither role.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CompilerInputRole {
    InterfaceOnly {
        interface: ArtifactId,
    },
    ReusableOriginal {
        interface: ArtifactId,
        original: ArtifactId,
    },
}

impl CompilerInputRole {
    pub fn interface(&self) -> ArtifactId {
        match self {
            Self::InterfaceOnly { interface } | Self::ReusableOriginal { interface, .. } => {
                *interface
            }
        }
    }

    pub fn original(&self) -> Option<ArtifactId> {
        match self {
            Self::InterfaceOnly { .. } => None,
            Self::ReusableOriginal { original, .. } => Some(*original),
        }
    }
}

/// One compiler namespace selected by accepted declarations and import proofs.
/// Availability and executable native demand remain in the retained view.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CompilerInputProjection {
    roles: BTreeMap<ExactModuleIdentity, CompilerInputRole>,
}

impl CompilerInputProjection {
    pub fn roles(&self) -> Vec<CompilerInputRole> {
        self.roles.values().cloned().collect()
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

    /// Issue only type roles from an authenticated interface closure. Native
    /// carriers in the view never become reuse offers through this operation.
    pub(crate) fn from_interface_view(view: &ArtifactView) -> Result<Self, CompileError> {
        let entries = view
            .entries()
            .into_iter()
            .filter(|entry| !entry.is_native())
            .collect::<Vec<_>>();
        Self::from_issued_entries(&entries)
    }

    /// Restrict already issued roles to a proved retained surface. An original
    /// absent from that surface loses its reuse offer, never its exact type role.
    pub(crate) fn within_view(&self, view: &ArtifactView) -> Self {
        let ids = view
            .descriptors()
            .into_iter()
            .map(|row| row.id)
            .collect::<BTreeSet<_>>();
        Self {
            roles: self
                .roles
                .iter()
                .filter_map(|(owner, role)| {
                    ids.contains(&role.interface()).then(|| {
                        let selected = match role.original() {
                            Some(original) if ids.contains(&original) => role.clone(),
                            _ => CompilerInputRole::InterfaceOnly {
                                interface: role.interface(),
                            },
                        };
                        (owner.clone(), selected)
                    })
                })
                .collect(),
        }
    }

    fn admit_role(
        &mut self,
        owner: ExactModuleIdentity,
        incoming: CompilerInputRole,
    ) -> Result<(), CompileError> {
        if let Some(previous) = self.roles.get_mut(&owner) {
            if previous.interface() != incoming.interface() {
                return Err(admission_failure(ArtifactInventoryFailure::OwnerConflict {
                    owner,
                }));
            }
            match (previous.original(), incoming.original()) {
                (Some(old), Some(new)) if old != new => {
                    return Err(admission_failure(
                        ArtifactInventoryFailure::CompilerOriginalOfferConflict {
                            owner,
                            existing: old,
                            incoming: new,
                        },
                    ));
                }
                (None, Some(_)) => *previous = incoming,
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
        Self {
            roles: self
                .roles
                .iter()
                .map(|(owner, role)| {
                    (
                        owner.clone(),
                        CompilerInputRole::InterfaceOnly {
                            interface: role.interface(),
                        },
                    )
                })
                .collect(),
        }
    }

    /// Importing a proved source surface preserves exact interface custody.
    /// Only its issued source roles offer native originals in the new compiler
    /// namespace; unrelated executable dependencies remain retained originals.
    pub(crate) fn for_source_owners(&self, owners: &BTreeSet<ExactModuleIdentity>) -> Self {
        Self {
            roles: self
                .roles
                .iter()
                .map(|(owner, role)| {
                    let selected = if owners.contains(owner) {
                        role.clone()
                    } else {
                        CompilerInputRole::InterfaceOnly {
                            interface: role.interface(),
                        }
                    };
                    (owner.clone(), selected)
                })
                .collect(),
        }
    }

    pub(crate) fn project_metadata(
        &self,
        mut metadata: ArtifactMetadataSnapshot,
    ) -> Result<ArtifactMetadataSnapshot, CompileError> {
        metadata.entries = self.entries_from_metadata(&metadata)?;
        metadata.ambiguous_native_owners.clear();
        Ok(metadata)
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
        metadata: &ArtifactMetadataSnapshot,
    ) -> Result<BTreeMap<ExactModuleIdentity, Arc<ArtifactEntry>>, CompileError> {
        let mut selected = BTreeMap::new();
        for (owner, role) in &self.roles {
            let interface = metadata
                .artifacts
                .get(&role.interface())
                .ok_or_else(|| failure("compiler interface role is outside retained custody"))?;
            if &interface.descriptor.owner != owner || interface.is_native() {
                return Err(failure("compiler interface role has another owner or kind"));
            }
            let entry = match role.original() {
                None => interface,
                Some(id) => {
                    let original = metadata.artifacts.get(&id).ok_or_else(|| {
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
        for entry in selected.values() {
            for required in &entry.requirements {
                let role = self.roles.get(required).ok_or_else(|| {
                    failure("compiler input projection omits an interface dependency")
                })?;
                if entry.interface_seals.get(required).is_some_and(|seal| {
                    metadata.artifacts[&role.interface()]
                        .descriptor
                        .interface_sha256
                        != *seal
                }) {
                    return Err(failure(
                        "compiler input projection changes an interface seal",
                    ));
                }
            }
        }
        Ok(selected)
    }
}

/// Native group demand checked against exact immutable artifact custody.
/// It is separate from the original offers needed before target compilation.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TargetNativeSelection {
    groups: BTreeSet<NativeGroupKey>,
}

impl TargetNativeSelection {
    pub fn groups(&self) -> &BTreeSet<NativeGroupKey> {
        &self.groups
    }
}

impl ArtifactView {
    /// Resolve a target's already certified roots inside this admitted view.
    /// This closes exact group edges; it does not grant a checked entry or infer
    /// target demand from the compiler's original offers.
    pub(crate) fn target_native_selection(
        &self,
        roots: &[NativeRequirementRoot],
    ) -> Result<TargetNativeSelection, CompileError> {
        let state = self.0.inventory.0.lock().expect("inventory lock");
        let owned = admitted_closure(&state, self.roots().into_iter());
        let mut selected = BTreeSet::new();
        for root in roots {
            let (artifact, ordinals) = match root {
                NativeRequirementRoot::AllGroups(artifact) => (
                    *artifact,
                    state
                        .payloads
                        .get(artifact)
                        .map(|entry| entry.native_group_ordinals.clone())
                        .unwrap_or_default(),
                ),
                NativeRequirementRoot::Group {
                    artifact,
                    original_ordinal,
                } => (*artifact, BTreeSet::from([*original_ordinal])),
            };
            if !owned.contains(&InventoryNodeKey::Artifact(artifact)) {
                return Err(admission_failure(
                    ArtifactInventoryFailure::NativeRootOutsideView { artifact },
                ));
            }
            for ordinal in ordinals {
                let key = NativeGroupKey {
                    artifact,
                    original_ordinal: ordinal,
                };
                if !owned.contains(&InventoryNodeKey::Group(key)) {
                    return Err(admission_failure(
                        ArtifactInventoryFailure::NativeGroupUnavailable {
                            artifact,
                            original_ordinal: ordinal,
                        },
                    ));
                }
                selected.insert(InventoryNodeKey::Group(key));
            }
        }
        let closure = admitted_closure(&state, selected.into_iter());
        Ok(TargetNativeSelection {
            groups: native_groups(&closure),
        })
    }
}
