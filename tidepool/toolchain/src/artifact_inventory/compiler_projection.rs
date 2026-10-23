//! Compiler input roles are issued independently of retained native custody.
//! Serialized roles become authority only after checking their exact originals.

use super::*;
use crate::exact_context::{
    RecordedCompilerRow, RecordedNativeChoices, RecordedOriginalProvenance,
    RecordedPublicationBirth,
};

/// Immutable publication observations in assembly/transport records. These
/// facts cannot issue the actual birth support held by a selected row.
#[derive(Clone, Debug)]
pub struct PublicationBirthFacts(Arc<crate::exact_context::PublicationNativeFacts>);
impl PublicationBirthFacts {
    pub(crate) fn facts(&self) -> &Arc<crate::exact_context::PublicationNativeFacts> {
        &self.0
    }
    pub(crate) fn capture(source: &CompilerSourceSupport) -> Result<Self, CompileError> {
        Ok(Self(Arc::clone(source.native_fact_owner())))
    }
    pub(crate) fn from_recorded_facts(
        facts: Arc<crate::exact_context::PublicationNativeFacts>,
    ) -> Self {
        Self(facts)
    }
}
impl PartialEq for PublicationBirthFacts {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(self.facts(), other.facts()) || self.facts() == other.facts()
    }
}
impl Eq for PublicationBirthFacts {}
impl Serialize for PublicationBirthFacts {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.facts().serialize(serializer)
    }
}
/// Persisted input-role facts. The interface is exact type custody; an original
/// offer additionally grants reuse of one authenticated immutable implementation.
/// Constructing or decoding this record grants neither role.
#[derive(Clone, Debug, Serialize)]
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
        native_authority: PublicationBirthFacts,
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
                    native_authority: la,
                },
                Self::PublishedSourceOriginal {
                    interface: ri,
                    original: ro,
                    source_revision: rr,
                    original_input_identity: rp,
                    selection: rs,
                    selection_sha256: rd,
                    native_authority: ra,
                },
            ) => {
                li == ri && lo == ro && lr == rr && lp == rp && ld == rd && la == ra
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
    /// Durable facts share the birth descriptor, never its per-view acquisition.
    fn recorded_row(&self) -> RecordedCompilerRow {
        match self {
            Self::InterfaceOnly { interface } => RecordedCompilerRow::InterfaceOnly {
                interface: *interface,
            },
            Self::ReusableOriginal {
                interface,
                original,
            } => RecordedCompilerRow::Original {
                interface: *interface,
                original: *original,
                provenance: RecordedOriginalProvenance::Reusable,
                native_choices: RecordedNativeChoices::Unissued,
            },
            Self::PublishedSourceOriginal {
                interface,
                original,
                source_revision,
                original_input_identity,
                selection,
                selection_sha256,
                native_authority,
            } => RecordedCompilerRow::Original {
                interface: *interface,
                original: *original,
                provenance: RecordedOriginalProvenance::Published(RecordedPublicationBirth {
                    source_revision: source_revision.clone(),
                    original_input_identity: original_input_identity.clone(),
                    selection: selection.clone(),
                    selection_sha256: *selection_sha256,
                    native_facts: Arc::clone(native_authority.facts()),
                }),
                native_choices: RecordedNativeChoices::Unissued,
            },
        }
    }

    pub(crate) fn recorded_fact(&self) -> RecordedCompilerRole {
        match self {
            Self::InterfaceOnly { interface } => RecordedCompilerRole::InterfaceOnly {
                interface: *interface,
            },
            Self::ReusableOriginal {
                interface,
                original,
            } => RecordedCompilerRole::ReusableOriginal {
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
                native_authority,
            } => RecordedCompilerRole::PublishedSourceOriginal {
                interface: *interface,
                original: *original,
                source_revision: source_revision.clone(),
                original_input_identity: original_input_identity.clone(),
                selection: selection.clone(),
                selection_sha256: *selection_sha256,
                native_authority: Arc::clone(native_authority.facts()),
            },
        }
    }

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
                native_authority,
            } => CompilerRoleObservation::PublishedSourceOriginal {
                interface: *interface,
                original: *original,
                source_revision,
                original_input_identity,
                selection: selection.observation_digest()?,
                selection_sha256: *selection_sha256,
                native_authority: native_authority.facts().commitment(),
            },
        })
    }
}

/// Authority-free observations shared by assembly and admitted rendering.
#[derive(Clone, Copy, Debug)]
pub struct CompilerRoleFacts<'a> {
    interface: ArtifactId,
    original: Option<ArtifactId>,
    publication: Option<PublicationFactsRef<'a>>,
}
#[derive(Clone, Copy, Debug)]
pub struct PublicationFactsRef<'a> {
    source_revision: &'a str,
    original_input_identity: &'a str,
    selection: &'a ExactArtifactSelection,
    selection_sha256: &'a [u8; 32],
    native_facts: &'a Arc<crate::exact_context::PublicationNativeFacts>,
}
impl<'a> PublicationFactsRef<'a> {
    pub fn source_revision(self) -> &'a str {
        self.source_revision
    }
    pub fn original_input_identity(self) -> &'a str {
        self.original_input_identity
    }
    pub fn selection(self) -> &'a ExactArtifactSelection {
        self.selection
    }
    pub fn selection_sha256(self) -> &'a [u8; 32] {
        self.selection_sha256
    }
    pub(crate) fn native_facts(self) -> &'a Arc<crate::exact_context::PublicationNativeFacts> {
        self.native_facts
    }
}
impl<'a> CompilerRoleFacts<'a> {
    fn from_assembly(role: &'a CompilerInputRole) -> Self {
        Self {
            interface: role.interface(),
            original: role.original(),
            publication: match role {
                CompilerInputRole::PublishedSourceOriginal {
                    source_revision,
                    original_input_identity,
                    selection,
                    selection_sha256,
                    native_authority,
                    ..
                } => Some(PublicationFactsRef {
                    source_revision,
                    original_input_identity,
                    selection,
                    selection_sha256,
                    native_facts: native_authority.facts(),
                }),
                _ => None,
            },
        }
    }
    fn from_admitted(row: &'a ResolvedCompilerRole) -> Self {
        Self {
            interface: row.interface().0.artifact,
            original: row.original().map(|(binding, _)| binding.artifact),
            publication: row.publication().map(|birth| {
                let facts = &birth.facts;
                PublicationFactsRef {
                    source_revision: &facts.source_revision,
                    original_input_identity: &facts.original_input_identity,
                    selection: &facts.selection,
                    selection_sha256: &facts.selection_sha256,
                    native_facts: &facts.native_facts,
                }
            }),
        }
    }
    pub fn interface(self) -> ArtifactId {
        self.interface
    }
    pub fn original(self) -> Option<ArtifactId> {
        self.original
    }
    pub fn publication(self) -> Option<PublicationFactsRef<'a>> {
        self.publication
    }
    pub fn is_published_source_original(self) -> bool {
        self.publication.is_some()
    }
}

/// Raw assembly observations cannot be supplied as admitted compiler authority.
#[derive(Clone, Copy)]
pub struct AssemblyRoleView<'a>(&'a CompilerInputProjection);
impl<'a> AssemblyRoleView<'a> {
    pub fn get(self, owner: &ExactModuleIdentity) -> Option<CompilerRoleFacts<'a>> {
        self.0
            .roles
            .get(owner)
            .map(CompilerRoleFacts::from_assembly)
    }
    pub fn iter(self) -> impl Iterator<Item = (&'a ExactModuleIdentity, CompilerRoleFacts<'a>)> {
        self.0
            .roles
            .iter()
            .map(|(owner, role)| (owner, CompilerRoleFacts::from_assembly(role)))
    }
    pub fn keys(self) -> impl Iterator<Item = &'a ExactModuleIdentity> {
        self.0.roles.keys()
    }
    pub fn len(self) -> usize {
        self.0.roles.len()
    }
    pub fn is_empty(self) -> bool {
        self.0.roles.is_empty()
    }
}

/// Only the actual selected row owner lends this admitted compiler surface.
#[derive(Clone, Copy)]
pub struct AdmittedCompilerRoles<'a> {
    selected: &'a SelectedCompilerRows,
}
impl<'a> AdmittedCompilerRoles<'a> {
    pub(crate) fn original_acquisition(
        self,
        owner: &ExactModuleIdentity,
    ) -> Option<&'a Arc<ArtifactEntry>> {
        self.selected
            .rows
            .get(owner)?
            .original()
            .map(|(_, entry)| entry)
    }

    pub(super) fn selected_row(
        self,
        owner: &ExactModuleIdentity,
    ) -> Option<&'a ResolvedCompilerRole> {
        self.selected.rows.get(owner)
    }

    pub(crate) fn native_decision(self, owner: &ExactModuleIdentity) -> InputNativeDecision<'a> {
        self.selected
            .rows
            .get(owner)
            .map_or(InputNativeDecision::UnissuedPolicy, |row| {
                row.native_decision()
            })
    }

    pub(super) fn selected_native_support(
        self,
        owner: &ExactModuleIdentity,
        group: NativeGroupBinding,
    ) -> Option<&'a SelectedNativeSupport> {
        self.selected
            .rows
            .get(owner)?
            .selected_native_support(group)
    }

    pub fn get(self, owner: &ExactModuleIdentity) -> Option<CompilerRoleFacts<'a>> {
        self.selected
            .rows
            .get(owner)
            .map(CompilerRoleFacts::from_admitted)
    }
    pub fn iter(self) -> impl Iterator<Item = (&'a ExactModuleIdentity, CompilerRoleFacts<'a>)> {
        self.selected
            .rows
            .iter()
            .map(|(owner, row)| (owner, CompilerRoleFacts::from_admitted(row)))
    }
    pub fn keys(self) -> impl Iterator<Item = &'a ExactModuleIdentity> {
        self.selected.rows.keys()
    }
    pub fn len(self) -> usize {
        self.selected.rows.len()
    }
    pub fn is_empty(self) -> bool {
        self.selected.rows.is_empty()
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
        native_authority: cid::Cid,
    },
}

/// Closed immutable role facts retain no per-view acquired input or payload
/// custody. Acquisition validates these facts against the receiving view.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum RecordedCompilerRole {
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
        native_authority: Arc<crate::exact_context::PublicationNativeFacts>,
    },
}

impl RecordedCompilerRole {
    fn matches_role_facts(&self, role: CompilerRoleFacts<'_>) -> bool {
        if self.interface() != role.interface() || self.original() != role.original() {
            return false;
        }
        match (self, role.publication()) {
            (
                Self::PublishedSourceOriginal {
                    source_revision,
                    original_input_identity,
                    selection,
                    selection_sha256,
                    native_authority,
                    ..
                },
                Some(publication),
            ) => {
                source_revision == publication.source_revision()
                    && original_input_identity == publication.original_input_identity()
                    && selection_sha256 == publication.selection_sha256()
                    && selection.snapshot() == publication.selection().snapshot()
                    && native_authority == publication.native_facts()
            }
            (Self::PublishedSourceOriginal { .. }, None) | (_, Some(_)) => false,
            (_, None) => true,
        }
    }

    /// Read the recorded interface fact without exposing the live role surface.
    pub(crate) fn interface(&self) -> ArtifactId {
        match self {
            Self::InterfaceOnly { interface }
            | Self::ReusableOriginal { interface, .. }
            | Self::PublishedSourceOriginal { interface, .. } => *interface,
        }
    }
    /// Read the recorded original fact without exposing the live role surface.
    pub(crate) fn original(&self) -> Option<ArtifactId> {
        match self {
            Self::InterfaceOnly { .. } => None,
            Self::ReusableOriginal { original, .. }
            | Self::PublishedSourceOriginal { original, .. } => Some(*original),
        }
    }
}
impl PartialEq for RecordedCompilerRole {
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
                    native_authority: la,
                },
                Self::PublishedSourceOriginal {
                    interface: ri,
                    original: ro,
                    source_revision: rr,
                    original_input_identity: rp,
                    selection: rs,
                    selection_sha256: rd,
                    native_authority: ra,
                },
            ) => {
                li == ri
                    && lo == ro
                    && lr == rr
                    && lp == rp
                    && ld == rd
                    && la == ra
                    && (Arc::ptr_eq(ls.snapshot(), rs.snapshot()) || ls.snapshot() == rs.snapshot())
            }
            _ => false,
        }
    }
}
impl Eq for RecordedCompilerRole {}
impl PartialEq<CompilerInputRole> for RecordedCompilerRole {
    fn eq(&self, other: &CompilerInputRole) -> bool {
        match (self, other) {
            (
                Self::InterfaceOnly { interface: left },
                CompilerInputRole::InterfaceOnly { interface: right },
            ) => left == right,
            (
                Self::ReusableOriginal {
                    interface: li,
                    original: lo,
                },
                CompilerInputRole::ReusableOriginal {
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
                    native_authority: la,
                },
                CompilerInputRole::PublishedSourceOriginal {
                    interface: ri,
                    original: ro,
                    source_revision: rr,
                    original_input_identity: rp,
                    selection: rs,
                    selection_sha256: rd,
                    native_authority: ra,
                },
            ) => {
                li == ri
                    && lo == ro
                    && lr == rr
                    && lp == rp
                    && ld == rd
                    && la == ra.facts()
                    && (Arc::ptr_eq(ls.snapshot(), rs.snapshot()) || ls.snapshot() == rs.snapshot())
            }
            _ => false,
        }
    }
}
impl PartialEq<RecordedCompilerRole> for CompilerInputRole {
    fn eq(&self, other: &RecordedCompilerRole) -> bool {
        other == self
    }
}

/// One compiler namespace selected by accepted declarations and import proofs.
/// Availability and executable native demand remain in the retained view.
#[derive(Clone, Debug, Default)]
pub struct CompilerInputProjection {
    roles: imbl::OrdMap<ExactModuleIdentity, CompilerInputRole>,
    // Updated only with changed roles. Snapshots borrow this pure fact root;
    // live roles separately retain their actual acquired publication owners.
    recorded_roles: imbl::OrdMap<ExactModuleIdentity, RecordedCompilerRow>,
}

impl PartialEq for CompilerInputProjection {
    fn eq(&self, other: &Self) -> bool {
        // Assembly compares role facts only. The derived encoding index also
        // carries native choices and cannot decide an acquired row's authority.
        self.roles == other.roles
    }
}
impl Eq for CompilerInputProjection {}

/// Acquired compiler-prefix code is retained with its exact occurrence grants.
/// Scalar records support persistence; equal records never replace live owners.
#[derive(Clone, Debug, Default)]
pub(crate) struct AcquiredNativeClosures {
    owners: imbl::OrdMap<[u8; 32], Arc<[Arc<crate::linked_native::AcquiredNativeClosure>]>>,
    components: crate::exact_context::NativeComponentRecords,
    recorded: crate::exact_context::NativeAcquiredClosureRecords,
    // Payload custody for persistence, never occurrence or executable permission.
    packages: imbl::OrdMap<ArtifactId, Arc<crate::certified_products::CertifiedPackageCodeProduct>>,
}

impl AcquiredNativeClosures {
    pub(crate) fn iter(
        &self,
    ) -> impl Iterator<Item = &Arc<crate::linked_native::AcquiredNativeClosure>> {
        self.owners.values().flat_map(|owners| owners.iter())
    }

    pub(crate) fn contains_owner(
        &self,
        owner: &Arc<crate::linked_native::AcquiredNativeClosure>,
    ) -> bool {
        self.owners
            .get(owner.identity())
            .is_some_and(|held| held.iter().any(|prior| Arc::ptr_eq(prior, owner)))
    }

    /// Authority-changing idempotence uses actual held owners. Snapshot data
    /// equality alone cannot discard another acquisition with equal code.
    pub(crate) fn same_held_owners(&self, other: &Self) -> bool {
        self.owners.ptr_eq(&other.owners)
            || (self.owners.len() == other.owners.len()
                && self.owners.iter().all(|(identity, owners)| {
                    other.owners.get(identity).is_some_and(|held| {
                        owners.len() == held.len()
                            && owners
                                .iter()
                                .all(|owner| held.iter().any(|prior| Arc::ptr_eq(prior, owner)))
                    })
                }))
    }

    /// Lend one intact issued witness for a new demand. The full HOME binding
    /// fixes source/retained edges; root issuance and closed code must agree.
    /// Package receipt cohort IDs may differ and do not choose a capability.
    /// Never mix child grants or reselect an already owned witness here. The
    /// returned prefix handle has no receiving-target Current correlation.
    pub(crate) fn group_owner(
        &self,
        binding: NativeGroupBinding,
        operation: &tidepool_repr::execution_schema::InventoryOperation,
    ) -> Result<Option<crate::linked_native::AcquiredNativeCode>, CompileError> {
        use crate::linked_native::NativeCodeGrant;
        let mut selected: Option<crate::linked_native::AcquiredNativeCode> = None;
        for closure in self.iter() {
            crate::host_work::checkpoint()?;
            operation
                .charge(1)
                .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
            let Some(incoming) = closure.home(binding) else {
                continue;
            };
            if let Some(prior) = &selected {
                let (
                    NativeCodeGrant::Home { product: held, .. },
                    NativeCodeGrant::Home {
                        product: incoming_product,
                        ..
                    },
                ) = (prior.grant(), incoming.grant())
                else {
                    return Err(failure("acquired HOME lookup changed occurrence kind"));
                };
                if !held.same_issuance(incoming_product) {
                    return Err(failure(
                        "acquired HOME binding has competing physical occurrences",
                    ));
                }
                if prior.code().id() != incoming.code().id() {
                    return Err(failure("acquired HOME binding has competing linked code"));
                }
            } else {
                selected = Some(incoming);
            }
        }
        Ok(selected)
    }

    fn one(owner: Arc<crate::linked_native::AcquiredNativeClosure>) -> Result<Self, CompileError> {
        // A successfully issued empty target has no historical CODE grants.
        // Its prepared body stays with the target rather than the compiler prefix.
        if owner.acquired().len() == 0 {
            return Ok(Self::default());
        }
        let mut components = crate::exact_context::NativeComponentRecords::new();
        let mut packages = imbl::OrdMap::new();
        for acquired in owner.acquired() {
            crate::host_work::checkpoint()?;
            if let crate::linked_native::NativeCodeGrant::Package { product, .. } = acquired.grant()
            {
                if let Some(prior) = packages.get(&product.artifact_id()) {
                    if !Self::same_package_payload(prior, product) {
                        return Err(failure("acquired package payload identity collision"));
                    }
                } else {
                    packages.insert(product.artifact_id(), Arc::clone(product));
                }
            }
            let component = acquired.code().component();
            if let Some(prior) = components.get(component.identity()) {
                if !Arc::ptr_eq(prior.facts(), component.snapshot_record())
                    && prior.facts().as_ref() != component.facts()
                {
                    return Err(failure("native component record commitment collision"));
                }
            } else {
                components.insert(
                    *component.identity(),
                    crate::exact_context::ImmutableNativeComponentRecordRef::from_component(
                        component,
                    ),
                );
            }
        }
        let identity = *owner.identity();
        let recorded = imbl::OrdMap::unit(
            identity,
            crate::exact_context::ImmutableNativeAcquiredClosureRecordRef::from_closure(&owner),
        );
        Ok(Self {
            owners: imbl::OrdMap::unit(identity, vec![owner].into()),
            components,
            recorded,
            packages,
        })
    }

    fn same_package_payload(
        left: &Arc<crate::certified_products::CertifiedPackageCodeProduct>,
        right: &Arc<crate::certified_products::CertifiedPackageCodeProduct>,
    ) -> bool {
        Arc::ptr_eq(left, right)
            || (left.descriptor() == right.descriptor()
                && left.product_sha256() == right.product_sha256()
                && left.envelope_sha256() == right.envelope_sha256()
                && left.code().owner() == right.code().owner())
    }

    /// Borrow actual acquired payload custody. Occurrence grants and ordered
    /// child permission remain in the owning closure and cannot be inferred here.
    pub(crate) fn package_product(
        &self,
        id: &ArtifactId,
    ) -> Option<&Arc<crate::certified_products::CertifiedPackageCodeProduct>> {
        self.packages.get(id)
    }

    pub(crate) fn package_products(
        &self,
    ) -> impl Iterator<Item = &Arc<crate::certified_products::CertifiedPackageCodeProduct>> {
        self.packages.values()
    }

    pub(crate) fn component_records(&self) -> &crate::exact_context::NativeComponentRecords {
        &self.components
    }

    pub(crate) fn closure_records(&self) -> &crate::exact_context::NativeAcquiredClosureRecords {
        &self.recorded
    }

    fn bind_record_baseline(
        &self,
        snapshot: &crate::exact_context::ExactContextSnapshot,
    ) -> Result<Self, CompileError> {
        if self.components != snapshot.native_components
            || self.recorded != snapshot.native_closures
        {
            return Err(failure(
                "restored native records differ from exact snapshot",
            ));
        }
        // Authentication already issued the live owners. Share their equal
        // immutable map bases without transferring any acquisition authority.
        let mut rebound = self.clone();
        rebound.components = snapshot.native_components.clone();
        rebound.recorded = snapshot.native_closures.clone();
        Ok(rebound)
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.owners.is_empty()
    }

    pub(crate) fn append(&self, incoming: &Self) -> Result<Self, CompileError> {
        if self.same_held_owners(incoming) {
            if !incoming.is_empty() {
                crate::host_work::checkpoint()?;
            }
            return Ok(self.clone());
        }
        let mut retained = self.clone();
        for (identity, owners) in &incoming.owners {
            crate::host_work::checkpoint()?;
            if let Some(prior) = retained.recorded.get(identity) {
                if prior != &incoming.recorded[identity] {
                    return Err(failure("acquired closure record commitment collision"));
                }
            } else {
                retained
                    .recorded
                    .insert(*identity, incoming.recorded[identity].clone());
            }
            let mut additions = Vec::new();
            for owner in owners.iter() {
                if !retained.contains_owner(owner) {
                    additions.push(Arc::clone(owner));
                }
            }
            if !additions.is_empty() {
                let mut held = retained
                    .owners
                    .get(identity)
                    .map_or_else(Vec::new, |held| held.to_vec());
                held.extend(additions);
                retained.owners.insert(*identity, held.into());
            }
        }
        for (identity, record) in &incoming.components {
            crate::host_work::checkpoint()?;
            if let Some(prior) = retained.components.get(identity) {
                if !Arc::ptr_eq(prior.facts(), record.facts()) && prior.facts() != record.facts() {
                    return Err(failure("native component record commitment collision"));
                }
            } else {
                retained.components.insert(*identity, record.clone());
            }
        }
        for (id, product) in &incoming.packages {
            crate::host_work::checkpoint()?;
            if let Some(prior) = retained.packages.get(id) {
                if !Self::same_package_payload(prior, product) {
                    return Err(failure("acquired package payload identity collision"));
                }
            } else {
                retained.packages.insert(*id, Arc::clone(product));
            }
        }
        Ok(retained)
    }
}

// Logical equality is durable data equality. Live-owner replacement and
// idempotence must use same_held_owners instead.
impl PartialEq for AcquiredNativeClosures {
    fn eq(&self, other: &Self) -> bool {
        self.recorded == other.recorded && self.components == other.components
    }
}
impl Eq for AcquiredNativeClosures {}

/// Complete only this acquired target closure. Available unused HOME groups
/// and unused captured package products do not become executable roots.
pub(crate) fn link_native_target(
    sources: &crate::artifacts::ResolvedNativeSourceProducts<'_>,
    output: &crate::artifacts::invocation_output::CapturedNativeOutput<'_>,
    products: &[Arc<crate::certified_products::CertifiedPackageCodeProduct>],
    selection: crate::certified_products::BoundPackageSelection,
    inherited: &AcquiredNativeClosures,
    operation: &tidepool_repr::execution_schema::InventoryOperation,
) -> Result<Arc<crate::linked_native::LinkedNativeTarget>, CompileError> {
    use crate::certified_products::{SelectedNativeImport, SelectedNativeTargetImport};
    use crate::linked_native::{
        resolve_package_export, AcquiredNativeClosure, AcquiredNativeCode, LinkedNativeTarget,
        LinkedNativeTargetImport, NativeAcquiredOccurrence, NativeCodeGrant, NativeOccurrenceId,
        NativeOccurrencePlan, NativePackageChoice, NativePackageReceipt,
    };
    use tidepool_repr::execution_schema::ValueId;
    use tidepool_repr::native_code::NativeRetainedRequirement;

    if !std::ptr::eq(output.operation().as_ref(), operation) {
        return Err(failure(
            "native linkage belongs to another output operation",
        ));
    }
    selection
        .require_output_operation(output.operation())
        .map_err(|error| CompileError::CompilerEvidence(Box::new(error)))?;
    let check_product = |product: &crate::certified_products::CertifiedPackageCodeProduct| {
        let owner = product.code().owner();
        if &owner.compiler_sha256 != output.producer_sha256()
            || &owner.profile_sha256 != output.profile_sha256()
        {
            return Err(failure(
                "package linkage differs from actual target compiler/profile",
            ));
        }
        Ok(())
    };
    let manifest = Arc::clone(selection.native_manifest());
    let stage_id = *manifest.stage_id();
    let occurrences = selection.occurrences();
    let mut by_envelope = BTreeMap::new();
    for product in products {
        if by_envelope
            .insert(*product.envelope_sha256(), product)
            .is_some()
        {
            return Err(failure(
                "captured output contains duplicate package envelope products",
            ));
        }
    }
    let mut occurrence_products = Vec::new();
    for row in occurrences {
        crate::host_work::checkpoint()?;
        let product = by_envelope
            .get(row.envelope_sha256())
            .copied()
            .ok_or_else(|| failure("selected occurrence lacks its authenticated product"))?;
        check_product(product)?;
        let owner = product.code().owner();
        if owner.unit != row.unit()
            || owner.module != row.module()
            || owner.interface_sha256 != *row.defining_interface_sha256()
            || owner.defining_version_sha256 != *row.defining_version_sha256()
            || product.code().globals().len() != row.imports().len()
        {
            return Err(failure(
                "package occurrence differs from its captured product contract",
            ));
        }
        occurrence_products.push(Arc::clone(product));
    }
    enum DraftPackage {
        Fresh(u32),
        Held(AcquiredNativeCode),
    }
    enum DraftTarget {
        Home(NativeGroupBinding, ValueId),
        Package(u32, ValueId),
        Retained(NativeRetainedRequirement),
    }
    let package_product = |occurrence: u32| {
        occurrence_products
            .get(occurrence as usize)
            .ok_or_else(|| failure("selected package occurrence is out of range"))
    };
    let home_rows = sources.groups().collect::<Vec<_>>();
    let home_grants = sources.acquired_homes()?;
    let homes = home_rows
        .iter()
        .enumerate()
        .map(|(index, row)| (row.binding(), index))
        .collect::<BTreeMap<_, _>>();
    let selected_groups = homes.keys().copied().collect::<BTreeSet<_>>();
    let original_packages = selection.checked_original_packages(operation)?;
    if !original_packages.use_roots().is_subset(&selected_groups) {
        return Err(failure(
            "Original package use exceeds selected exact HOME groups",
        ));
    }
    let mut consumed_original_slots = BTreeSet::new();
    let mut fresh_roots = BTreeSet::new();
    let mut current = BTreeMap::new();
    let mut home_drafts = Vec::new();
    let mut prior_owners = Vec::new();
    let mut held_nodes = BTreeMap::<NativeOccurrenceId, NativeAcquiredOccurrence>::new();
    let mut artifacts = sources.acquired_artifacts().inventory().empty_view();
    for row in &home_rows {
        crate::host_work::checkpoint()?;
        let prior = inherited.group_owner(row.binding(), operation)?;
        if let Some(prior) = &prior {
            let NativeCodeGrant::Home { product, .. } = prior.grant() else {
                return Err(failure("completed HOME occurrence changed its grant kind"));
            };
            if !product.same_issuance(row.group()) {
                return Err(failure(
                    "completed HOME occurrence changed its exact product",
                ));
            }
        }
        if matches!(
            row.source_association(),
            super::ResolvedSourceAssociation::Current
        ) {
            current.insert(row.binding(), Arc::clone(row.shared_group()));
        }
        let previous = prior
            .as_ref()
            .map(|owner| owner.imports().collect::<Vec<_>>());
        let contract = selection
            .applicable_home_contract(row)
            .map_err(|error| CompileError::CompilerEvidence(Box::new(error)))?;
        let home_links = contract.as_ref().map(|contract| contract.links());
        let definitions = row.group().group().definitions();
        if row.imports().len() != definitions.globals().len()
            || previous
                .as_ref()
                .is_some_and(|links| links.len() != row.imports().len())
        {
            return Err(failure(
                "HOME ordered slots differ from their physical definitions",
            ));
        }
        let mut drafts = BTreeMap::new();
        let mut package_cursor = 0;
        for (slot, (link, global)) in row.imports().iter().zip(definitions.globals()).enumerate() {
            match link {
                SelectedNativeImport::Source { binding, .. } => {
                    if !homes.contains_key(binding) {
                        return Err(failure("selected HOME child is absent"));
                    }
                }
                SelectedNativeImport::Retained { .. } => {}
                SelectedNativeImport::Package {
                    identity,
                    interface_digest,
                } => {
                    if identity != &global.identity {
                        return Err(failure("HOME package slot differs from its global"));
                    }
                    let slot_number = u32::try_from(slot)
                        .map_err(|_| failure("HOME package slot exceeds u32"))?;
                    let original = original_packages
                        .slot(row.binding(), slot_number, identity, interface_digest)
                        .map_err(|error| CompileError::CompilerEvidence(Box::new(error)))?;
                    if original.is_some() {
                        consumed_original_slots.insert((row.binding(), slot_number));
                    }
                    let home = match home_links {
                        Some(links) => {
                            let link = links
                                .get(package_cursor)
                                .ok_or_else(|| failure("HOME package contract is too short"))?;
                            package_cursor += 1;
                            Some(link)
                        }
                        None => None,
                    };
                    if home
                        .zip(original)
                        .is_some_and(|(home, original)| home != original)
                    {
                        return Err(failure(
                            "HOME and Original choices conflict on one exact slot",
                        ));
                    }
                    let draft = if let Some(selected) = home.or(original) {
                        let occurrence = selected.occurrence();
                        package_product(occurrence)?;
                        fresh_roots.insert(occurrence);
                        DraftPackage::Fresh(occurrence)
                    } else {
                        let old = previous
                            .as_ref()
                            .and_then(|links| links.get(slot))
                            .ok_or_else(|| {
                                failure("HOME package slot lacks current or exact held occurrence")
                            })?;
                        let crate::linked_native::AcquiredNativeImport::Code { acquired, .. } = old
                        else {
                            return Err(failure("held HOME package slot changed its import kind"));
                        };
                        let NativeCodeGrant::Package { product, .. } = acquired.grant() else {
                            return Err(failure(
                                "held HOME package slot lacks its exact package grant",
                            ));
                        };
                        check_product(product)?;
                        artifacts = artifacts.retain_custody(acquired.closure().artifacts())?;
                        for retained in acquired.selected_occurrences() {
                            if let Some(old) = held_nodes.get(&retained.occurrence()) {
                                if !old.same_linkage(&retained) {
                                    return Err(failure(
                                        "held acquired occurrence has competing exact linkage",
                                    ));
                                }
                            } else {
                                held_nodes.insert(retained.occurrence(), retained);
                            }
                        }
                        DraftPackage::Held(acquired.clone())
                    };
                    drafts.insert(slot, draft);
                }
            }
        }
        if home_links.is_some_and(|links| package_cursor != links.len()) {
            return Err(failure(
                "HOME package contract has unconsumed selected slots",
            ));
        }
        prior_owners.push(prior);
        home_drafts.push(drafts);
    }
    if original_packages.coordinates().collect::<BTreeSet<_>>() != consumed_original_slots {
        return Err(failure("Original package proof has unused selected slots"));
    }
    let prepared = Arc::clone(selection.prepared_program());
    let target_definitions = prepared.definitions();
    if sources.target_requirements().len() != target_definitions.globals().len() {
        return Err(failure(
            "target slots differ from the paired physical program",
        ));
    }
    let mut target_drafts = Vec::new();
    let mut target_cursor = 0;
    for (link, global) in sources
        .target_requirements()
        .iter()
        .zip(target_definitions.globals())
    {
        let draft = match link {
            SelectedNativeTargetImport::Source { binding, value } => {
                if !homes.contains_key(binding) {
                    return Err(failure("target lacks its exact HOME root"));
                }
                DraftTarget::Home(*binding, *value)
            }
            SelectedNativeTargetImport::RetainedCapture {
                identity,
                generation,
            } => {
                if identity != &global.identity || global.required_generation != Some(*generation) {
                    return Err(failure(
                        "target retained capture differs from its issued global",
                    ));
                }
                DraftTarget::Retained(NativeRetainedRequirement::new(
                    identity.clone(),
                    *generation,
                    None,
                ))
            }
            SelectedNativeTargetImport::Package {
                identity,
                interface_digest,
            } => {
                if identity != &global.identity {
                    return Err(failure("target package slot changed its global"));
                }
                let link = selection
                    .target_roots()
                    .get(target_cursor)
                    .ok_or_else(|| failure("target package slot lacks its exact manifest root"))?;
                target_cursor += 1;
                let occurrence = link.occurrence();
                let value = resolve_package_export(
                    package_product(occurrence)?,
                    global,
                    target_definitions,
                    interface_digest,
                )?;
                fresh_roots.insert(occurrence);
                DraftTarget::Package(occurrence, value)
            }
        };
        target_drafts.push(draft);
    }
    if target_cursor != selection.target_roots().len() {
        return Err(failure("target package roots were not fully consumed"));
    }
    let mut reached = BTreeSet::new();
    let mut pending = fresh_roots.into_iter().collect::<Vec<_>>();
    while let Some(occurrence) = pending.pop() {
        crate::host_work::checkpoint()?;
        operation
            .charge(1)
            .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
        if !reached.insert(occurrence) {
            continue;
        }
        let row = occurrences
            .get(occurrence as usize)
            .ok_or_else(|| failure("package occurrence edge is out of range"))?;
        pending.extend(row.imports().iter().map(|link| link.occurrence()));
    }
    let acquired_products = reached
        .iter()
        .map(|occurrence| {
            (
                *occurrence,
                Arc::clone(&occurrence_products[*occurrence as usize]),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let receipt = NativePackageReceipt::issue(manifest, &acquired_products, operation)?;
    let mut plans = Vec::new();
    for (row, drafts) in home_rows.iter().zip(home_drafts) {
        operation
            .reserve::<(usize, NativePackageChoice)>(drafts.len())
            .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
        let packages = drafts
            .into_iter()
            .map(|(slot, draft)| {
                let choice = match draft {
                    DraftPackage::Fresh(occurrence) => NativePackageChoice::Receipt(
                        receipt
                            .occurrence(occurrence)
                            .ok_or_else(|| failure("selected package acquisition is absent"))?,
                    ),
                    DraftPackage::Held(acquired) => NativePackageChoice::Held(acquired),
                };
                Ok((slot, choice))
            })
            .collect::<Result<BTreeMap<_, _>, CompileError>>()?;
        let grant = home_grants
            .get(&row.binding())
            .cloned()
            .ok_or_else(|| failure("selected HOME grant is absent"))?;
        plans.push(NativeOccurrencePlan::home(grant, packages, operation)?);
    }
    for occurrence in &reached {
        plans.push(NativeOccurrencePlan::package(
            receipt
                .occurrence(*occurrence)
                .ok_or_else(|| failure("selected package acquisition is absent"))?,
            operation,
        )?);
    }
    crate::host_work::checkpoint()?;
    let linked = NativeOccurrencePlan::link(plans, operation)?;
    crate::host_work::checkpoint()?;
    let mut nodes = held_nodes;
    for index in 0..home_rows.len() {
        if let Some(prior) = &prior_owners[index] {
            if prior.code() != linked[index].code() {
                return Err(failure("selected HOME head changed its exact closed code"));
            }
            // Equal code does not reissue its authorizing child occurrences.
            // Current source correlation is borrowed separately by this target.
            artifacts = artifacts.retain_custody(prior.closure().artifacts())?;
            for original in prior.selected_occurrences() {
                if let Some(previous) = nodes.get(&original.occurrence()) {
                    if !previous.same_linkage(&original) {
                        return Err(failure(
                            "selected HOME history has competing exact occurrence grants",
                        ));
                    }
                } else {
                    nodes.insert(original.occurrence(), original);
                }
            }
        } else {
            nodes.insert(linked[index].occurrence(), linked[index].clone());
        }
    }
    for (offset, _) in reached.iter().enumerate() {
        let node = linked[home_rows.len() + offset].clone();
        let id = node.occurrence();
        if let Some(prior) = nodes.get(&id) {
            if !prior.same_linkage(&node) {
                return Err(failure(
                    "package receipt occurrence changed its closed code",
                ));
            }
        } else {
            nodes.insert(id, node);
        }
    }
    // Fresh manifest choices used only to corroborate a held HOME head are not
    // executable demand. Preserve only actual selected HOME and target roots.
    let mut selected = BTreeSet::new();
    let mut pending = selected_groups
        .iter()
        .copied()
        .map(NativeOccurrenceId::Home)
        .collect::<Vec<_>>();
    pending.extend(target_drafts.iter().filter_map(|draft| match draft {
        DraftTarget::Package(occurrence, _) => Some(NativeOccurrenceId::Package {
            stage_id,
            occurrence: *occurrence,
        }),
        _ => None,
    }));
    while let Some(occurrence) = pending.pop() {
        crate::host_work::checkpoint()?;
        if !selected.insert(occurrence) {
            continue;
        }
        let node = nodes
            .get(&occurrence)
            .ok_or_else(|| failure("selected exact occurrence is absent"))?;
        pending.extend(node.children());
    }
    nodes.retain(|occurrence, _| selected.contains(occurrence));
    let closure = AcquiredNativeClosure::from_occurrences(
        nodes.into_values().collect(),
        artifacts,
        operation,
    )?;
    let current = Arc::new(current);
    let target_imports = target_drafts
        .into_iter()
        .map(|draft| match draft {
            DraftTarget::Home(binding, value) => closure
                .home(binding)
                .map(|code| LinkedNativeTargetImport::Code {
                    acquired: code.reborrow(&current),
                    value,
                })
                .ok_or_else(|| failure("issued target lost its HOME root")),
            DraftTarget::Package(occurrence, value) => closure
                .get(NativeOccurrenceId::Package {
                    stage_id,
                    occurrence,
                })
                .map(|code| LinkedNativeTargetImport::Code {
                    acquired: code.reborrow(&current),
                    value,
                })
                .ok_or_else(|| failure("issued target lost its package root")),
            DraftTarget::Retained(requirement) => {
                Ok(LinkedNativeTargetImport::RetainedCapture(requirement))
            }
        })
        .collect::<Result<Vec<_>, CompileError>>()?;
    Ok(Arc::new(LinkedNativeTarget {
        prepared,
        imports: target_imports.into(),
        closure,
        sources: current,
    }))
}

/// Matched compiler roles and exact custody selected by their existing issuer.
/// Required type interfaces do not grant lexical imports or native body demand.
#[derive(Clone, Debug)]
pub(crate) struct CompilerSourceSupport {
    projection: CompilerInputProjection,
    artifacts: ArtifactView,
    native_fact_owner: Arc<crate::exact_context::PublicationNativeFacts>,
    native_offers: Arc<[IssuedNativeOffer]>,
    native_closures: AcquiredNativeClosures,
    native_offer_facts: imbl::OrdMap<[u8; 32], crate::exact_context::NativeOfferSnapshotFacts>,
    native_availability: crate::declaration_context::RequestNativeAvailability,
    selected: Arc<SelectedCompilerRows>,
    input_decisions: IssuedInputNativeDecisions,
}

/// Assembly-only state for one compiler support. Incomplete assembly never
/// escapes this module: every usable [`CompilerSourceSupport`] is produced by
/// [`SupportDraft::close`]. Every accumulated completion already names custody
/// retained in the draft's `artifacts`; constructors merge completion custody
/// before the draft exists and `close` never re-folds it. The prior fact owner
/// is carried only as the maintenance input.
struct SupportDraft {
    projection: CompilerInputProjection,
    artifacts: ArtifactView,
    native_fact_owner: Option<Arc<crate::exact_context::PublicationNativeFacts>>,
    native_offers: Arc<[IssuedNativeOffer]>,
    native_closures: AcquiredNativeClosures,
    native_offer_facts: imbl::OrdMap<[u8; 32], crate::exact_context::NativeOfferSnapshotFacts>,
    native_availability: crate::declaration_context::RequestNativeAvailability,
    selected: Arc<SelectedCompilerRows>,
    input_decisions: IssuedInputNativeDecisions,
}

impl SupportDraft {
    /// Close assembly without changing any selected acquisition. Completion
    /// custody is already retained in `artifacts`; native facts are maintained
    /// from the explicit selected owners.
    fn close(self) -> Result<CompilerSourceSupport, CompileError> {
        let SupportDraft {
            mut projection,
            artifacts,
            native_fact_owner,
            native_offers,
            native_closures,
            native_offer_facts,
            native_availability,
            mut selected,
            mut input_decisions,
        } = self;
        let changed = selected.changed_owners.clone();
        for owner in &changed {
            match selected.rows.get(owner) {
                Some(row) => {
                    projection.roles.insert(owner.clone(), row.assembly_role());
                    projection
                        .recorded_roles
                        .insert(owner.clone(), row.recorded_row());
                    if let Some(fact) = row.recorded_decision() {
                        input_decisions.facts.insert(owner.clone(), fact);
                    } else {
                        input_decisions.facts.remove(owner);
                    }
                }
                None => {
                    projection.roles.remove(owner);
                    projection.recorded_roles.remove(owner);
                    input_decisions.facts.remove(owner);
                }
            }
        }
        if !changed.is_empty() {
            Arc::make_mut(&mut selected).changed_owners.clear();
        }
        let native_fact_owner = crate::exact_context::PublicationNativeFacts::maintain(
            native_fact_owner.as_ref(),
            crate::exact_context::PublicationNativeMaps {
                roles: projection.recorded_roles.clone(),
                offers: native_offer_facts.clone(),
                native_availability: native_availability.persistent_facts().clone(),
            },
        )
        .map_err(|error| failure(&error.to_string()))?;
        Ok(CompilerSourceSupport {
            projection,
            artifacts,
            native_fact_owner,
            native_offers,
            native_closures,
            native_offer_facts,
            native_availability,
            selected,
            input_decisions,
        })
    }

    /// Merge completion custody into the draft. Completion-set bookkeeping and
    /// pairing against the final acquired entries happen once in [`close`].
    /// Only the incoming completions merge custody here; repeated enrichment
    /// never re-folds the accumulated set, and [`close`] performs the single
    /// final pairing pass.
    fn append_acquired_closures(
        &mut self,
        incoming: &AcquiredNativeClosures,
    ) -> Result<(), CompileError> {
        let completions = self.native_closures.append(incoming)?;
        for completion in incoming.iter() {
            self.artifacts = self.artifacts.retain_custody(completion.artifacts())?;
        }
        self.native_closures = completions;
        Ok(())
    }

    /// Restore the draft invariant after a narrowing operation: every carried
    /// completion and every issued native permission must still name custody
    /// owned by the final artifact view.
    fn retain_runtime_custody(
        mut self,
        availability_source: &ArtifactView,
    ) -> Result<Self, CompileError> {
        let native_custody =
            availability_source.retain_native_availability_custody(&self.native_availability)?;
        self.artifacts = self.artifacts.retain_custody(&native_custody)?;
        self.retain_completion_custody()
    }

    /// Completed targets retain their actually acquired groups. Source support
    /// keeps unconsumed availability in its opaque offers instead of expanding
    /// that permission into the receiving graph.
    fn retain_completion_custody(mut self) -> Result<Self, CompileError> {
        for completion in self.native_closures.iter() {
            crate::host_work::checkpoint()?;
            self.artifacts = self.artifacts.retain_custody(completion.artifacts())?;
        }
        Ok(self)
    }
}

/// One authenticated program insertion retains whether the existing full
/// qualifier was required by a change from type-only to an Original role.
pub(crate) struct ProgramSupportDelta {
    pub(crate) support: CompilerSourceSupport,
    pub(crate) changed_owners: Vec<ExactModuleIdentity>,
    pub(crate) promotion_qualifier_invocations: usize,
}

/// An operation's role closure borrows its original complete issuer. Native
/// availability, input choices and instance authority remain with that issuer.
pub(crate) struct DemandCompilerProjection {
    selected: SelectedCompilerRows,
    source_support: Arc<CompilerSourceSupport>,
    projection: CompilerInputProjection,
    metadata: CompilerArtifactMetadata,
    owners: BTreeSet<ExactModuleIdentity>,
    work: DemandCompilerProjectionWork,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct DemandCompilerProjectionWork {
    pub(crate) owner_lookups: usize,
    pub(crate) payload_lookups: usize,
    pub(crate) canonical_requirement_edges: usize,
    pub(crate) original_requirement_edges: usize,
    pub(crate) selected_rows: usize,
    pub(crate) metadata_entries: usize,
}

impl DemandCompilerProjection {
    pub(crate) fn compiler_roles(&self) -> AdmittedCompilerRoles<'_> {
        self.selected.admitted_roles()
    }

    pub(crate) fn source_support(&self) -> &Arc<CompilerSourceSupport> {
        &self.source_support
    }

    pub(crate) fn projection(&self) -> &CompilerInputProjection {
        &self.projection
    }

    pub(crate) fn metadata(&self) -> &CompilerArtifactMetadata {
        &self.metadata
    }

    pub(crate) fn owners(&self) -> &BTreeSet<ExactModuleIdentity> {
        &self.owners
    }

    pub(crate) fn work(&self) -> DemandCompilerProjectionWork {
        self.work
    }
}

impl CompilerSourceSupport {
    pub(crate) fn preserves_selected_rows_from(
        &self,
        baseline: &Self,
    ) -> Result<bool, CompileError> {
        for (owner, previous) in baseline.projection.snapshot_roles() {
            let Some(role) = self.projection.roles.get(owner) else {
                return Ok(false);
            };
            if matches!(
                CompilerInputProjection::choose_role(owner, previous, role)?,
                CompilerRoleIssuer::Receiving
            ) && !baseline.selected[owner].same_selected_acquisition(&self.selected[owner])
            {
                return Ok(false);
            }
        }
        Ok(true)
    }

    pub(crate) fn same_selected_acquisitions(&self, other: &Self) -> bool {
        self.selected.rows.len() == other.selected.rows.len()
            && self.selected.rows.iter().all(|(owner, row)| {
                other
                    .selected
                    .rows
                    .get(owner)
                    .is_some_and(|other| row.same_selected_acquisition(other))
            })
    }

    pub(crate) fn same_recorded_facts(&self, other: &Self) -> bool {
        // Offers are the exact identity/mode/proof/custody set persisted by the
        // existing typed map. Live insertion order is not part of that contract.
        // Borrow its maintained root; comparison never sorts or re-encodes it.
        self.projection == other.projection
            && self.artifacts == other.artifacts
            && self.native_fact_owner == other.native_fact_owner
            && self.native_closures == other.native_closures
            && self.native_offer_facts == other.native_offer_facts
            && self
                .native_availability
                .same_recorded_facts(&other.native_availability)
            && self.selected.same_recorded_facts(&other.selected)
            && self.input_decisions.recorded_facts() == other.input_decisions.recorded_facts()
    }
}

/// A recorded choice is not authority until its role and exact Group references
/// have been authenticated with the source acquisition owner.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RecordedInputNativeDecision {
    pub(crate) role: RecordedCompilerRole,
    #[serde(with = "crate::exact_context::serde_ord_map")]
    pub(crate) groups: imbl::OrdMap<u32, NativeGroupBinding>,
}

/// Presence includes an explicitly empty choice. Absence leaves the original
/// under its existing input policy; neither case advertises native availability.
#[derive(Clone, Debug, Default)]
pub(crate) struct IssuedInputNativeDecisions {
    pub(super) facts: imbl::OrdMap<ExactModuleIdentity, RecordedInputNativeDecision>,
}

/// Live acquisitions of actually selected support are separate from graph facts.
/// Missing support is sticky even when another selected source supplies a live owner.
#[derive(Clone, Debug)]
pub(super) enum SelectedNativeSupport {
    Acquired(Arc<[IssuedNativeOffer]>),
    Missing(Arc<[IssuedNativeOffer]>),
}

impl SelectedNativeSupport {
    pub(super) fn missing() -> Self {
        Self::Missing(Arc::from([]))
    }

    pub(super) fn acquisitions(&self) -> &[IssuedNativeOffer] {
        match self {
            Self::Acquired(offers) | Self::Missing(offers) => offers,
        }
    }

    fn same_acquisition(&self, other: &Self) -> bool {
        matches!(
            (self, other),
            (Self::Missing(_), Self::Missing(_)) | (Self::Acquired(_), Self::Acquired(_))
        ) && self.acquisitions().len() == other.acquisitions().len()
            && self
                .acquisitions()
                .iter()
                .zip(other.acquisitions())
                .all(|(left, right)| left.same_issuance(right))
    }

    pub(super) fn merge(&mut self, other: &Self) -> bool {
        let missing = matches!(self, Self::Missing(_)) || matches!(other, Self::Missing(_));
        let mut acquired = self.acquisitions().to_vec();
        let mut changed = false;
        for offer in other.acquisitions() {
            if !acquired.iter().any(|prior| prior.same_issuance(offer)) {
                acquired.push(offer.clone());
                changed = true;
            }
        }
        if changed || missing != matches!(self, Self::Missing(_)) {
            *self = if missing {
                Self::Missing(acquired.into())
            } else {
                Self::Acquired(acquired.into())
            };
            true
        } else {
            false
        }
    }
}

/// This is a carried choice constraint, never native availability or demand.
pub(crate) enum InputNativeDecision<'a> {
    UnissuedPolicy,
    IssuedChoices(&'a imbl::OrdMap<u32, NativeGroupBinding>),
}

impl IssuedInputNativeDecisions {
    fn with_recorded_facts(
        &self,
        facts: imbl::OrdMap<ExactModuleIdentity, RecordedInputNativeDecision>,
    ) -> Self {
        Self { facts }
    }

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
                    return Err(refusal(InputDecisionRefusal::MissingDecision));
                }
            };
            if !matches!(role, CompilerInputRole::ReusableOriginal { .. }) {
                return Err(refusal(InputDecisionRefusal::RoleMismatch));
            }
            result.facts.insert(
                owner.clone(),
                RecordedInputNativeDecision {
                    role: role.recorded_fact(),
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
        let mut facts = self.facts.clone();
        for (owner, fact) in &self.facts {
            if projection
                .snapshot_roles()
                .get(owner)
                .is_none_or(|role| &fact.role != role)
            {
                facts.remove(owner);
            }
        }
        self.with_recorded_facts(facts)
    }
    pub(crate) fn restricted(
        &self,
        projection: &CompilerInputProjection,
        view: &ArtifactView,
    ) -> Self {
        let roots = view.roots();
        let mut facts = self.facts.clone();
        for (owner, fact) in &self.facts {
            if projection
                .snapshot_roles()
                .get(owner)
                .is_none_or(|role| &fact.role != role)
            {
                facts.remove(owner);
                continue;
            }
            let mut groups = fact.groups.clone();
            for (ordinal, group) in &fact.groups {
                if !roots.contains(&InventoryNodeKey::Group(*group)) {
                    groups.remove(ordinal);
                }
            }
            if fact.groups.diff(&groups).next().is_some() {
                facts.insert(
                    owner.clone(),
                    RecordedInputNativeDecision {
                        role: fact.role.clone(),
                        groups,
                    },
                );
            }
        }
        self.with_recorded_facts(facts)
    }
}

/// Native permission belongs to the selected Original row, including an issued
/// empty choice and missing acquisition evidence after recorded recovery.
#[derive(Clone, Debug)]
enum SelectedInputNativeChoice {
    Unissued,
    Issued {
        groups: imbl::OrdMap<u32, NativeGroupBinding>,
        support: imbl::OrdMap<NativeGroupBinding, SelectedNativeSupport>,
    },
}

/// The private checked relation between one selected Original and its birth.
#[derive(Clone, Debug)]
struct PublishedOriginal {
    facts: crate::exact_context::RecordedPublicationBirth,
    birth: Arc<CompilerSourceSupport>,
}

impl PublishedOriginal {
    fn from_endorsement(
        owner: &ExactModuleIdentity,
        role: &CompilerInputRole,
        birth: &Arc<CompilerSourceSupport>,
    ) -> Result<Option<Arc<Self>>, CompileError> {
        let CompilerInputRole::PublishedSourceOriginal {
            source_revision,
            original_input_identity,
            selection,
            selection_sha256,
            native_authority,
            ..
        } = role
        else {
            return Ok(None);
        };
        let expected = birth
            .compiler_roles()
            .get(owner)
            .ok_or_else(|| failure("publication birth lacks its selected Original"))?;
        if expected.interface() != role.interface()
            || expected.original() != role.original()
            || expected.publication().is_some()
            || birth.artifacts.selection_snapshot().as_ref() != selection.snapshot().as_ref()
            || birth.native_fact_owner().as_ref() != native_authority.facts().as_ref()
        {
            return Err(failure(
                "publication birth differs from its exact pre-endorsement Original",
            ));
        }
        Ok(Some(Arc::new(Self {
            facts: crate::exact_context::RecordedPublicationBirth {
                source_revision: source_revision.clone(),
                original_input_identity: original_input_identity.clone(),
                selection: selection.clone(),
                selection_sha256: *selection_sha256,
                native_facts: Arc::clone(birth.native_fact_owner()),
            },
            birth: Arc::clone(birth),
        })))
    }
}

#[derive(Clone, Debug)]
enum OriginalProvenance {
    Reusable,
    Published(Arc<PublishedOriginal>),
}

/// Exact binding observations retained from successful compiler-role admission.
/// These entries are acquisition owners, not a reconstruction from payload IDs.
type AcquiredCompilerBinding = (ArtifactBinding, Arc<ArtifactEntry>);

#[derive(Debug)]
pub(crate) struct ResolvedCompilerRole(SelectedCompilerRow);

#[derive(Clone, Debug)]
enum SelectedCompilerRow {
    InterfaceOnly {
        interface: AcquiredCompilerBinding,
    },
    Original {
        interface: AcquiredCompilerBinding,
        original: AcquiredCompilerBinding,
        provenance: OriginalProvenance,
        native_choice: SelectedInputNativeChoice,
    },
}

impl ResolvedCompilerRole {
    fn assembly_role(&self) -> CompilerInputRole {
        let interface = self.interface().0.artifact;
        match &self.0 {
            SelectedCompilerRow::InterfaceOnly { .. } => {
                CompilerInputRole::InterfaceOnly { interface }
            }
            SelectedCompilerRow::Original {
                original,
                provenance: OriginalProvenance::Reusable,
                ..
            } => CompilerInputRole::ReusableOriginal {
                interface,
                original: original.0.artifact,
            },
            SelectedCompilerRow::Original {
                original,
                provenance: OriginalProvenance::Published(birth),
                ..
            } => CompilerInputRole::PublishedSourceOriginal {
                interface,
                original: original.0.artifact,
                source_revision: birth.facts.source_revision.clone(),
                original_input_identity: birth.facts.original_input_identity.clone(),
                selection: birth.facts.selection.clone(),
                selection_sha256: birth.facts.selection_sha256,
                native_authority: PublicationBirthFacts::from_recorded_facts(Arc::clone(
                    &birth.facts.native_facts,
                )),
            },
        }
    }

    fn recorded_row(&self) -> RecordedCompilerRow {
        let mut row = self.assembly_role().recorded_row();
        if let RecordedCompilerRow::Original { native_choices, .. } = &mut row {
            *native_choices = match self.native_decision() {
                InputNativeDecision::UnissuedPolicy => RecordedNativeChoices::Unissued,
                InputNativeDecision::IssuedChoices(groups) => RecordedNativeChoices::Issued {
                    groups: groups.clone(),
                },
            };
        }
        row
    }

    pub(crate) fn same_recorded_facts(&self, other: &Self) -> bool {
        self.interface().0 == other.interface().0
            && self.original().map(|(binding, _)| binding)
                == other.original().map(|(binding, _)| binding)
            && match (self.native_decision(), other.native_decision()) {
                (InputNativeDecision::UnissuedPolicy, InputNativeDecision::UnissuedPolicy) => true,
                (
                    InputNativeDecision::IssuedChoices(left),
                    InputNativeDecision::IssuedChoices(right),
                ) => left == right,
                _ => false,
            }
            && match (self.publication(), other.publication()) {
                (None, None) => true,
                (Some(left), Some(right)) => left.facts == right.facts,
                _ => false,
            }
    }
    fn publication(&self) -> Option<&Arc<PublishedOriginal>> {
        match &self.0 {
            SelectedCompilerRow::Original {
                provenance: OriginalProvenance::Published(birth),
                ..
            } => Some(birth),
            _ => None,
        }
    }
    fn with_issued_provenance(mut self, role: &CompilerInputRole) -> Result<Self, CompileError> {
        if role.is_published_source_original() {
            if self.publication().is_none()
                || role.recorded_row() != self.assembly_role().recorded_row()
            {
                return Err(failure(
                    "assembly publication facts cannot replace the selected birth",
                ));
            }
        } else if let SelectedCompilerRow::Original { provenance, .. } = &mut self.0 {
            *provenance = OriginalProvenance::Reusable;
        }
        Ok(self)
    }

    pub(super) fn selected_bindings(&self) -> impl Iterator<Item = ArtifactBinding> + '_ {
        self.bindings().map(|(binding, _)| *binding)
    }
}
impl Clone for ResolvedCompilerRole {
    fn clone(&self) -> Self {
        #[cfg(test)]
        type_query_work::cloned_row();
        Self(self.0.clone())
    }
}

/// The solver's chosen bindings and their acquired payloads, not raw projection authority.
/// Raw merge callers do not request or allocate these rows.
pub(super) struct ResolvedCompilerInputs {
    pub(super) artifacts: ArtifactView,
    pub(super) projection: CompilerInputProjection,
    pub(super) selected: BTreeMap<ExactModuleIdentity, ResolvedCompilerRole>,
    pub(super) input_decisions: IssuedInputNativeDecisions,
}

#[derive(Clone, Copy)]
pub(super) enum CompilerRoleIssuer {
    Receiving,
    Incoming,
}

pub(super) struct SelectedCompilerRole<'a> {
    pub(super) owner: &'a ExactModuleIdentity,
    pub(super) role: &'a CompilerInputRole,
    pub(super) issuer: CompilerRoleIssuer,
}

impl ResolvedCompilerRole {
    fn from_issued_bindings(
        interface: AcquiredCompilerBinding,
        original: Option<AcquiredCompilerBinding>,
    ) -> Self {
        Self(match original {
            Some(original) => SelectedCompilerRow::Original {
                interface,
                original,
                provenance: OriginalProvenance::Reusable,
                native_choice: SelectedInputNativeChoice::Unissued,
            },
            None => SelectedCompilerRow::InterfaceOnly { interface },
        })
    }

    fn recorded_decision(&self) -> Option<RecordedInputNativeDecision> {
        match self.native_decision() {
            InputNativeDecision::UnissuedPolicy => None,
            InputNativeDecision::IssuedChoices(groups) => Some(RecordedInputNativeDecision {
                role: self.assembly_role().recorded_fact(),
                groups: groups.clone(),
            }),
        }
    }

    pub(super) fn native_decision(&self) -> InputNativeDecision<'_> {
        match &self.0 {
            SelectedCompilerRow::Original {
                native_choice: SelectedInputNativeChoice::Issued { groups, .. },
                ..
            } => InputNativeDecision::IssuedChoices(groups),
            _ => InputNativeDecision::UnissuedPolicy,
        }
    }

    fn selected_native_support(&self, group: NativeGroupBinding) -> Option<&SelectedNativeSupport> {
        match &self.0 {
            SelectedCompilerRow::Original {
                native_choice: SelectedInputNativeChoice::Issued { support, .. },
                ..
            } => support.get(&group),
            _ => None,
        }
    }

    /// Only an actual issuer or recorded authentication may install a choice.
    /// General custody attachment never calls this operation.
    fn with_native_choice(
        mut self,
        groups: imbl::OrdMap<u32, NativeGroupBinding>,
        acquire: impl Fn(NativeGroupBinding) -> SelectedNativeSupport,
    ) -> Self {
        if let SelectedCompilerRow::Original { native_choice, .. } = &mut self.0 {
            let support = groups
                .values()
                .copied()
                .map(|group| (group, acquire(group)))
                .collect();
            *native_choice = SelectedInputNativeChoice::Issued { groups, support };
        }
        self
    }

    fn narrow_native_choice(&mut self, view: &ArtifactView) {
        if let SelectedCompilerRow::Original {
            native_choice: SelectedInputNativeChoice::Issued { groups, support },
            ..
        } = &mut self.0
        {
            let removed = groups
                .iter()
                .filter(|(_, group)| !view.roots().contains(&InventoryNodeKey::Group(**group)))
                .map(|(ordinal, group)| (*ordinal, *group))
                .collect::<Vec<_>>();
            for (ordinal, group) in removed {
                groups.remove(&ordinal);
                support.remove(&group);
            }
        }
    }

    fn same_selected_acquisition(&self, other: &Self) -> bool {
        let binding = |left: &AcquiredCompilerBinding, right: &AcquiredCompilerBinding| {
            left.0 == right.0 && left.1.same_issuing_authority(&right.1)
        };
        binding(self.interface(), other.interface())
            && match (self.original(), other.original()) {
                (None, None) => true,
                (Some(left), Some(right)) => binding(left, right),
                _ => false,
            }
            && match (&self.0, &other.0) {
                (
                    SelectedCompilerRow::Original {
                        provenance: left_provenance,
                        native_choice: left,
                        ..
                    },
                    SelectedCompilerRow::Original {
                        provenance: right_provenance,
                        native_choice: right,
                        ..
                    },
                ) => {
                    let provenance = match (left_provenance, right_provenance) {
                        (OriginalProvenance::Reusable, OriginalProvenance::Reusable) => true,
                        (
                            OriginalProvenance::Published(left),
                            OriginalProvenance::Published(right),
                        ) => Arc::ptr_eq(&left.birth, &right.birth),
                        _ => false,
                    };
                    provenance
                        && match (left, right) {
                            (
                                SelectedInputNativeChoice::Unissued,
                                SelectedInputNativeChoice::Unissued,
                            ) => true,
                            (
                                SelectedInputNativeChoice::Issued {
                                    groups: left_groups,
                                    support: left,
                                },
                                SelectedInputNativeChoice::Issued {
                                    groups: right_groups,
                                    support: right,
                                },
                            ) => {
                                left_groups == right_groups
                                    && left.iter().all(|(group, selected)| {
                                        right
                                            .get(group)
                                            .is_some_and(|other| selected.same_acquisition(other))
                                    })
                            }
                            _ => false,
                        }
                }
                (
                    SelectedCompilerRow::InterfaceOnly { .. },
                    SelectedCompilerRow::InterfaceOnly { .. },
                ) => true,
                _ => false,
            }
    }

    fn interface(&self) -> &AcquiredCompilerBinding {
        match &self.0 {
            SelectedCompilerRow::InterfaceOnly { interface }
            | SelectedCompilerRow::Original { interface, .. } => interface,
        }
    }

    fn original(&self) -> Option<&AcquiredCompilerBinding> {
        match &self.0 {
            SelectedCompilerRow::InterfaceOnly { .. } => None,
            SelectedCompilerRow::Original { original, .. } => Some(original),
        }
    }

    fn bindings(&self) -> impl Iterator<Item = &AcquiredCompilerBinding> {
        std::iter::once(self.interface()).chain(self.original())
    }

    fn select_type_only(&mut self) {
        if let SelectedCompilerRow::Original { interface, .. } = &self.0 {
            self.0 = SelectedCompilerRow::InterfaceOnly {
                interface: interface.clone(),
            };
        }
    }

    fn validate_original_pair(
        state: &InventoryState,
        canonical: ArtifactBinding,
        carrier: ArtifactBinding,
    ) -> Result<(), CompileError> {
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
        Ok(())
    }
}

#[cfg(test)]
mod type_query_work {
    #[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
    pub(super) struct Counts {
        pub census_rows: usize,
        pub iterated_rows: usize,
        pub lookups: usize,
        pub resolved_rows: usize,
        pub canonical_checks: usize,
        pub cloned_rows: usize,
        pub certificate_hashes: usize,
        pub descriptor_serializations: usize,
    }

    thread_local! {
        static COUNTS: std::cell::Cell<Counts> = const { std::cell::Cell::new(Counts {
            census_rows: 0, iterated_rows: 0, lookups: 0, resolved_rows: 0,
            canonical_checks: 0,
            cloned_rows: 0,
            certificate_hashes: 0,
            descriptor_serializations: 0,
        }) };
    }

    pub(super) fn take() -> Counts {
        COUNTS.with(|counts| counts.replace(Counts::default()))
    }
    fn observe(update: impl FnOnce(&mut Counts)) {
        COUNTS.with(|counts| {
            let mut value = counts.get();
            update(&mut value);
            counts.set(value);
        });
    }
    pub(super) fn census_row() {
        observe(|counts| counts.census_rows += 1);
    }
    pub(super) fn iterated_row() {
        observe(|counts| counts.iterated_rows += 1);
    }
    pub(super) fn lookup() {
        observe(|counts| counts.lookups += 1);
    }
    pub(super) fn resolved_row() {
        observe(|counts| counts.resolved_rows += 1);
    }
    pub(super) fn canonical_check() {
        observe(|counts| counts.canonical_checks += 1);
    }
    pub(super) fn cloned_row() {
        observe(|counts| counts.cloned_rows += 1);
    }
    pub(super) fn canonical_descriptor_rebuild() {
        observe(|counts| {
            counts.certificate_hashes += 1;
            counts.descriptor_serializations += 1;
        });
    }
}

/// Canonical classification belongs to the exact admitted rows. A raw role
/// projection cannot supply home units independently of those interfaces.
/// Persistent rows let a publication share unchanged exact bindings with forks.
#[derive(Clone, Debug)]
pub(crate) struct SelectedCompilerRows {
    rows: imbl::OrdMap<ExactModuleIdentity, ResolvedCompilerRole>,
    home_units: Arc<imbl::OrdSet<String>>,
    changed_owners: BTreeSet<ExactModuleIdentity>,
}

impl SelectedCompilerRows {
    pub(crate) fn admitted_roles(&self) -> AdmittedCompilerRoles<'_> {
        AdmittedCompilerRoles { selected: self }
    }

    fn decision_facts(&self) -> IssuedInputNativeDecisions {
        IssuedInputNativeDecisions {
            facts: self
                .rows
                .iter()
                .filter_map(|(owner, row)| {
                    row.recorded_decision()
                        .map(|decision| (owner.clone(), decision))
                })
                .collect(),
        }
    }

    fn same_recorded_facts(&self, other: &Self) -> bool {
        self.home_units == other.home_units
            && self.rows.len() == other.rows.len()
            && self.rows.iter().all(|(owner, row)| {
                other
                    .rows
                    .get(owner)
                    .is_some_and(|other| row.same_recorded_facts(other))
            })
    }

    fn from_admitted(rows: BTreeMap<ExactModuleIdentity, ResolvedCompilerRole>) -> Self {
        let mut home_units = BTreeSet::new();
        for row in rows.values() {
            #[cfg(test)]
            type_query_work::census_row();
            if let ArtifactPayload::Canonical(interface) = &row.interface().1.payload {
                home_units.extend(interface.home_units().iter().cloned());
            }
        }
        Self {
            changed_owners: rows.keys().cloned().collect(),
            rows: rows.into_iter().collect(),
            home_units: Arc::new(home_units.into_iter().collect()),
        }
    }
}

impl SelectedCompilerRows {
    fn get(&self, owner: &ExactModuleIdentity) -> Option<&ResolvedCompilerRole> {
        #[cfg(test)]
        type_query_work::lookup();
        self.rows.get(owner)
    }

    fn iter(&self) -> impl Iterator<Item = (&ExactModuleIdentity, &ResolvedCompilerRole)> {
        self.rows.iter().inspect(|_| {
            #[cfg(test)]
            type_query_work::iterated_row();
        })
    }

    fn values(&self) -> impl Iterator<Item = &ResolvedCompilerRole> {
        self.iter().map(|(_, row)| row)
    }
}

impl std::ops::Index<&ExactModuleIdentity> for SelectedCompilerRows {
    type Output = ResolvedCompilerRole;

    fn index(&self, owner: &ExactModuleIdentity) -> &Self::Output {
        self.get(owner).expect("admitted compiler role")
    }
}

/// The acquired pairs chosen by one issued primary role. Equal image bytes
/// do not replace these exact binding selection witnesses.
pub(crate) struct WorldPrimaryTypeRow<'a> {
    pub(crate) owner: &'a ExactModuleIdentity,
    pub(crate) interface_binding: &'a ArtifactBinding,
    pub(crate) original_binding: Option<&'a ArtifactBinding>,
    pub(crate) transport_binding: &'a ArtifactBinding,
    pub(crate) entry: &'a Arc<ArtifactEntry>,
}

/// A borrowed acquired row and the exact source that issued its bindings.
/// Consumers cannot pair its Original acquisition with another source view.
pub(crate) enum CompilerPrimaryInput<'a> {
    InterfaceOnly(CompilerPrimaryRow<'a>),
    Original(CompilerPrimaryRow<'a>),
}

pub(crate) struct CompilerPrimaryRow<'a> {
    artifacts: &'a ArtifactView,
    owner: &'a ExactModuleIdentity,
    row: &'a ResolvedCompilerRole,
}

impl<'a> CompilerPrimaryInput<'a> {
    pub(super) fn artifacts(&self) -> &'a ArtifactView {
        self.row().artifacts
    }

    fn row(&self) -> &CompilerPrimaryRow<'a> {
        match self {
            Self::InterfaceOnly(row) | Self::Original(row) => row,
        }
    }
    pub(crate) fn owner(&self) -> &'a ExactModuleIdentity {
        self.row().owner
    }
    pub(crate) fn interface(&self) -> ArtifactBinding {
        self.row().row.interface().0
    }
    pub(crate) fn original(&self) -> Option<ArtifactBinding> {
        self.row().row.original().map(|(binding, _)| *binding)
    }
    pub(crate) fn entry(&self) -> &'a Arc<ArtifactEntry> {
        let row = self.row().row;
        &row.original().unwrap_or(row.interface()).1
    }
}

/// Borrowed selected interface observations and the same issuing view that
/// resolves their scoped bindings. Historical custody cannot choose a row.
pub(crate) struct SelectedCompilerInterfaces<'a> {
    artifacts: &'a ArtifactView,
    selected: &'a SelectedCompilerRows,
}

enum InterfaceProjectionPurpose {
    PreserveCustody,
    TypeNames,
}

impl SelectedCompilerInterfaces<'_> {
    pub(crate) fn get(&self, owner: &ExactModuleIdentity) -> Option<&Arc<ArtifactEntry>> {
        self.selected.get(owner).map(|row| &row.interface().1)
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (&ExactModuleIdentity, &Arc<ArtifactEntry>)> {
        self.selected
            .iter()
            .map(|(owner, row)| (owner, &row.interface().1))
    }

    pub(crate) fn values(&self) -> impl Iterator<Item = &Arc<ArtifactEntry>> {
        self.iter().map(|(_, entry)| entry)
    }

    pub(crate) fn is_home_unit(&self, unit: &str) -> bool {
        self.selected.home_units.contains(unit)
    }

    pub(crate) fn support_for_owners(
        &self,
        owners: Vec<ExactModuleIdentity>,
    ) -> Result<CompilerSourceSupport, CompileError> {
        self.project_interfaces(owners, InterfaceProjectionPurpose::PreserveCustody)
    }

    /// Only the signature/witness-authenticated type boundary requests this
    /// projection. Its execution owner is retained separately by that caller.
    pub(crate) fn type_interfaces_for_owners(
        &self,
        owners: Vec<ExactModuleIdentity>,
    ) -> Result<CompilerSourceSupport, CompileError> {
        self.project_interfaces(owners, InterfaceProjectionPurpose::TypeNames)
    }

    fn project_interfaces(
        &self,
        owners: Vec<ExactModuleIdentity>,
        purpose: InterfaceProjectionPurpose,
    ) -> Result<CompilerSourceSupport, CompileError> {
        let mut projection = CompilerInputProjection::default();
        let mut pending = owners;
        while let Some(owner) = pending.pop() {
            if projection.roles.contains_key(&owner) {
                continue;
            }
            let entry = self
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
        let selected = projection
            .roles
            .keys()
            .map(|owner| {
                let mut row = self.selected[owner].clone();
                row.select_type_only();
                (owner.clone(), row)
            })
            .collect::<BTreeMap<_, _>>();
        // The rows retain their issuer's exact bindings. Demotion still checks
        // the canonical interface's own dependency closure and seals.
        let entries = selected
            .values()
            .map(|row| (row.interface().0.artifact, Arc::clone(&row.interface().1)))
            .collect();
        projection.entries_from_artifacts(&entries)?;
        let artifacts = match purpose {
            InterfaceProjectionPurpose::PreserveCustody => {
                self.select_owners(projection.roles.keys().cloned().collect())?
            }
            InterfaceProjectionPurpose::TypeNames => {
                // The validated type closure already has exact admitted rows.
                // Execution remains with the original source/checked-value owner.
                let empty = self.artifacts.inventory().empty_view();
                if selected.is_empty() {
                    empty
                } else {
                    let additions = selected
                        .values()
                        .map(|row| (self.artifacts.clone(), row.clone()))
                        .collect::<Vec<_>>();
                    empty.append_type_interfaces(&additions)?
                }
            }
        };
        SupportDraft {
            native_availability: Default::default(),
            projection,
            artifacts,
            native_offers: Arc::from([]),
            native_fact_owner: None,
            native_offer_facts: Default::default(),
            native_closures: AcquiredNativeClosures::default(),
            selected: Arc::new(SelectedCompilerRows::from_admitted(selected)),
            input_decisions: IssuedInputNativeDecisions::default(),
        }
        .close()
    }

    pub(crate) fn select_owners(
        &self,
        owners: Vec<ExactModuleIdentity>,
    ) -> Result<ArtifactView, CompileError> {
        let ids = owners
            .iter()
            .map(|owner| {
                self.get(owner)
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
        let roots = owners
            .iter()
            .map(|owner| InventoryNodeKey::Artifact(self.selected[owner].interface().0))
            .collect::<Vec<_>>();
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

impl ArtifactView {
    /// Reuse the exact issued canonical rows and proof owners. This operation
    /// adds no native roots, decisions, materialization inventory or new scope.
    fn append_type_interfaces<'a>(
        &self,
        additions: impl IntoIterator<Item = &'a (ArtifactView, ResolvedCompilerRole)>,
    ) -> Result<Self, CompileError> {
        self.append_admitted_custody(
            additions.into_iter().map(|(source, row)| {
                (source.lease.as_ref(), row.interface().0, &row.interface().1)
            }),
            &[],
        )
    }

    /// Append exact admitted bindings and the custody of opaque native offers.
    /// Group proof scopes stay distinct from their artifact-carrier scopes;
    /// executable roots remain with the issuing offers.
    fn append_admitted_custody<'a>(
        &self,
        bindings: impl IntoIterator<Item = (&'a ViewLease, ArtifactBinding, &'a Arc<ArtifactEntry>)>,
        offers: &[IssuedNativeOffer],
    ) -> Result<Self, CompileError> {
        let mut selection = self.lease.selection.as_ref().clone();
        let mut payloads = self.lease.payloads.clone();
        let mut acquired_owner_counts = self.lease.acquired_owner_counts.clone();
        let mut pending_scopes = Vec::new();
        let mut scope_sources = BTreeMap::new();
        let mut introduced = Vec::new();
        let mut materialization_sources = self.lease.materialization_sources.clone();
        let mut materialization_parents = BTreeSet::from([self.lease.materialization.id()]);
        let mut retain_payload = |binding: ArtifactBinding, entry: &Arc<ArtifactEntry>| {
            let retained = match payloads.get(&binding.artifact) {
                Some(previous) => ArtifactEntry::merge_catalog_input_custody(previous, entry)?,
                None => Arc::clone(entry),
            };
            if payloads
                .get(&binding.artifact)
                .is_none_or(|prior| !Arc::ptr_eq(prior, &retained))
            {
                ViewLease::insert_payload(
                    &mut payloads,
                    &mut acquired_owner_counts,
                    binding.artifact,
                    Arc::clone(&retained),
                );
                introduced.push((binding.artifact, retained));
            }
            Ok::<_, CompileError>(())
        };
        for (source, binding, entry) in bindings {
            if selection
                .namespace
                .get(&binding.artifact)
                .is_some_and(|prior| *prior != binding)
            {
                return Err(admission_failure(
                    ArtifactInventoryFailure::BindingAmbiguity {
                        artifact: binding.artifact,
                    },
                ));
            }
            selection.namespace.insert(binding.artifact, binding);
            selection
                .roots
                .insert(InventoryNodeKey::Artifact(binding), ());
            retain_payload(binding, entry)?;
            pending_scopes.push((binding.selection, source));
        }
        for offer in offers {
            let source = offer.custody_lease().as_ref();
            for carrier in offer.artifact_carriers() {
                let entry = source
                    .payloads
                    .get(&carrier.artifact)
                    .ok_or_else(|| failure("issued native carrier lacks its acquired payload"))?;
                retain_payload(*carrier, entry)?;
                selection
                    .custody_roots
                    .insert(InventoryNodeKey::Artifact(*carrier), ());
            }
            pending_scopes.extend(offer.required_scopes().iter().map(|scope| (*scope, source)));
            materialization_sources
                .insert(source.materialization.id(), source.materialization.clone());
            materialization_parents.insert(source.materialization.id());
        }
        while let Some((scope, source)) = pending_scopes.pop() {
            let proof = source
                .selection
                .scopes
                .get(&scope)
                .ok_or_else(|| failure("type-interface binding lacks its issuing proof"))?;
            if let Some(prior) = selection.scopes.get(&scope) {
                if prior != proof {
                    return Err(failure(
                        "type-interface scope has differing immutable proofs",
                    ));
                }
                continue;
            }
            pending_scopes.extend(
                proof
                    .direct_ancestors()
                    .iter()
                    .copied()
                    .map(|ancestor| (ancestor, source)),
            );
            selection.scopes.insert(scope, proof.clone());
            scope_sources.insert(scope, proof.clone());
        }
        {
            let mut state = self.lease.inventory.0.lock().expect("inventory lock");
            let proofs = scope_proof::prepare_issued_install(&state, scope_sources.into_values())?;
            scope_proof::install(&mut state, proofs);
            for (id, entry) in introduced {
                state.intern(id, &entry);
            }
        }
        materialization_sources.insert(
            self.lease.materialization.id(),
            self.lease.materialization.clone(),
        );
        // New canonical rows carry their immutable bytes in the actual payload
        // owners. An incoming view's unrelated cached physical files are not
        // required to materialize those rows.
        let lease = Arc::new(ViewLease {
            admission_roots: self.lease.admission_roots.clone(),
            selection: Arc::new(selection),
            payloads,
            acquired_owner_counts,
            inventory: self.lease.inventory.clone(),
            materialization_sources,
            materialization: MaterializationSource {
                cache: Arc::new(Mutex::new(BTreeMap::new())),
                parents: materialization_parents.into_iter().collect(),
                fixed: Default::default(),
            },
        });
        // This existing cleanup also releases strong snapshot owners belonging
        // to dead views. Its registry scan is separate from keyed role work.
        Ok(Self::from_custody(lease))
    }
}

impl crate::declaration_context::RequestCompilerInputs {
    /// Project demand from this request's paired admitted support and metadata.
    /// This does not select native groups, change lexical scope, or replace
    /// the issuer's instance proofs.
    pub(crate) fn demanded_compiler_projection(
        &self,
        roots: &BTreeSet<ExactModuleIdentity>,
        operation: &tidepool_repr::execution_schema::InventoryOperation,
    ) -> Result<DemandCompilerProjection, CompileError> {
        crate::host_work::checkpoint()?;
        let source = self.shared_compiler_source_support();
        let retained = &self.metadata().retained;
        let charge = |work: Result<(), tidepool_repr::execution_schema::ParseError>| {
            work.map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))
        };
        // Pending edges borrow immutable owners. Reserve twice the slots to
        // cover the vector's capacity growth before copying any references.
        charge(operation.reserve::<(&ExactModuleIdentity, &ExactModuleIdentity)>(roots.len()))?;
        let mut projection = CompilerInputProjection::default();
        let mut selected = BTreeMap::new();
        let mut owners = BTreeSet::<ExactModuleIdentity>::new();
        let mut pending = roots.iter().collect::<Vec<_>>();
        let mut work = DemandCompilerProjectionWork::default();
        while let Some(owner) = pending.pop() {
            crate::host_work::checkpoint()?;
            charge(operation.charge(1))?;
            if owners.contains(owner) {
                continue;
            }
            work.owner_lookups += 2;
            let role = source
                .projection
                .roles
                .get(owner)
                .ok_or_else(|| failure("demanded compiler owner lacks its issued role"))?;
            let row = source
                .selected
                .get(owner)
                .ok_or_else(|| failure("demanded compiler owner lacks its admitted row"))?;
            if row.interface().0.artifact != role.interface()
                || &row.interface().1.descriptor.owner != owner
                || !row.interface().1.is_compiler_interface()
                || row.original().map(|(binding, _)| binding.artifact) != role.original()
                || row.original().is_some_and(|(_, entry)| {
                    &entry.descriptor.owner != owner || !entry.is_native()
                })
            {
                return Err(failure(
                    "demanded compiler role differs from its admitted row",
                ));
            }
            // One set key and four map keys, plus conservative node/link
            // storage. All String and role copies are charged before cloning.
            charge(operation.reserve::<(ExactModuleIdentity, [usize; 4])>(1))?;
            charge(
                operation.reserve::<(ExactModuleIdentity, ResolvedCompilerRole, [usize; 8])>(1),
            )?;
            charge(operation.reserve::<(ExactModuleIdentity, CompilerInputRole, [usize; 8])>(1))?;
            charge(
                operation.reserve::<(ExactModuleIdentity, RecordedCompilerRole, [usize; 8])>(1),
            )?;
            charge(operation.reserve::<(ExactModuleIdentity, Arc<ArtifactEntry>, [usize; 4])>(1))?;
            for _ in 0..5 {
                charge(operation.charge(owner.unit.len()))?;
                charge(operation.charge(owner.module.len()))?;
            }
            if let CompilerInputRole::PublishedSourceOriginal {
                source_revision,
                original_input_identity,
                ..
            } = role
            {
                // Admission retains the live role and its closed recorded fact.
                for _ in 0..2 {
                    charge(operation.charge(source_revision.len()))?;
                    charge(operation.charge(original_input_identity.len()))?;
                }
            }
            for (original, (binding, entry)) in std::iter::once((false, row.interface()))
                .chain(row.original().map(|entry| (true, entry)))
            {
                crate::host_work::checkpoint()?;
                work.payload_lookups += 1;
                let acquired = retained.artifacts.get(&binding.artifact).ok_or_else(|| {
                    failure("demanded compiler payload is outside request custody")
                })?;
                if binding.artifact != entry.descriptor.id || !Arc::ptr_eq(acquired, entry) {
                    return Err(failure(
                        "demanded compiler row differs from its acquired request payload",
                    ));
                }
                if original {
                    work.original_requirement_edges += entry.requirements.len();
                } else {
                    work.canonical_requirement_edges += entry.requirements.len();
                }
                charge(
                    operation.reserve::<(&ExactModuleIdentity, &ExactModuleIdentity)>(
                        entry.requirements.len(),
                    ),
                )?;
                for required in &entry.requirements {
                    crate::host_work::checkpoint()?;
                    pending.push(required);
                }
            }
            if let Some((carrier, _)) = row.original() {
                let state = source
                    .artifacts
                    .lease
                    .inventory
                    .0
                    .lock()
                    .expect("inventory lock");
                ResolvedCompilerRole::validate_original_pair(&state, row.interface().0, *carrier)?;
            }
            owners.insert(owner.clone());
            projection.admit_role(owner.clone(), role.clone())?;
            selected.insert(owner.clone(), row.clone());
        }
        // The exact row and acquired payload handles already own canonical/native
        // admission. Rebuilding their descriptors would repeat certificate
        // hashing and encoding without acquiring any additional authority.
        let mut entries = BTreeMap::new();
        for (owner, row) in &selected {
            crate::host_work::checkpoint()?;
            // Canonical requirements remain obligations even when this owner's
            // selected Original has a different interface-use census.
            projection.validate_requirements(
                row.bindings().map(|(_, entry)| entry.as_ref()),
                |required| selected.get(required).map(|row| row.interface().1.as_ref()),
            )?;
            entries.insert(
                owner.clone(),
                Arc::clone(&row.original().unwrap_or(row.interface()).1),
            );
        }
        work.selected_rows = selected.len();
        work.metadata_entries = entries.len();
        Ok(DemandCompilerProjection {
            selected: SelectedCompilerRows::from_admitted(selected),
            source_support: source,
            projection,
            metadata: CompilerArtifactMetadata {
                entries: entries.into_iter().collect(),
                retained: Arc::clone(retained),
            },
            owners,
            work,
        })
    }
}

impl CompilerSourceSupport {
    pub(crate) fn publication_birth(&self, owner: &ExactModuleIdentity) -> Option<&Arc<Self>> {
        self.selected
            .rows
            .get(owner)?
            .publication()
            .map(|publication| &publication.birth)
    }

    pub(crate) fn permits_publication_target_native_groups(
        &self,
        owner: &ExactModuleIdentity,
        groups: &BTreeSet<NativeGroupBinding>,
    ) -> Result<bool, CompileError> {
        let birth = self
            .publication_birth(owner)
            .ok_or_else(|| failure("selected compiler row lacks publication birth"))?;
        let originals = birth
            .compiler_roles()
            .iter()
            .filter_map(|(_, role)| role.original())
            .collect::<BTreeSet<_>>();
        Ok(groups.iter().all(|group| {
            match birth
                .native_availability
                .issued_row(group.binding.artifact())
            {
                Some(row) => row.exact_group_support(*group).is_some(),
                None => !originals.contains(&group.binding.artifact()),
            }
        }))
    }

    pub(crate) fn recorded_rows(&self) -> &imbl::OrdMap<ExactModuleIdentity, RecordedCompilerRow> {
        &self.native_fact_owner.roles
    }

    pub(crate) fn compiler_roles(&self) -> AdmittedCompilerRoles<'_> {
        self.selected.admitted_roles()
    }

    /// Borrow metadata from the actual selected acquisitions. Catalog payload
    /// representatives cannot replace a selected Original at this boundary.
    pub(crate) fn compiler_metadata(&self) -> CompilerArtifactMetadata {
        CompilerArtifactMetadata {
            entries: self
                .selected
                .iter()
                .map(|(owner, row)| {
                    (
                        owner.clone(),
                        Arc::clone(&row.original().unwrap_or(row.interface()).1),
                    )
                })
                .collect(),
            retained: self.artifacts.metadata_snapshot(),
        }
    }

    /// A completed output authenticates each selected row here. Required TYPE
    /// dependencies may belong to the receiving baseline; request closure is
    /// checked when that baseline and this fragment are composed.
    pub(crate) fn from_certified_output_fragment(
        selection: &crate::certified_products::CertifiedSourceSelection,
        artifacts: &ArtifactView,
    ) -> Result<Self, CompileError> {
        if selection.has_unissued_current_native_products() {
            return Err(failure(
                "current native products lack physical cohort issuance",
            ));
        }
        let projection = selection
            .compiler_projection(artifacts)
            .map_err(|error| CompileError::CompilerEvidence(Box::new(error)))?;
        let selected = Self::certified_output_rows(selection, &projection, artifacts)?;
        let availability = crate::declaration_context::RequestNativeAvailability::from_issued_rows(
            selection.issued_native_availability().cloned(),
        )?;
        let mut offers = Vec::new();
        for row in availability.rows() {
            for offer in row.offers() {
                if !offers
                    .iter()
                    .any(|prior: &IssuedNativeOffer| prior.same_issuance(offer))
                {
                    offers.push(offer.clone());
                }
            }
        }
        for role in projection.roles() {
            if role
                .original()
                .is_some_and(|original| !offers.iter().any(|offer| offer.original() == original))
            {
                return Err(failure(
                    "source selection lost its issued native availability",
                ));
            }
        }
        let input_decisions =
            IssuedInputNativeDecisions::from_certified_continuation(selection, &projection)?;
        let mut timing = crate::timing::InvocationTiming::start_scoped(
            "exact.compiler_support.output_fragment",
            "retain_issuance",
        );
        let result = Self::complete_resolved(
            ResolvedCompilerInputs {
                artifacts: artifacts.clone(),
                projection,
                selected,
                input_decisions,
            },
            offers,
            availability,
            selection.native_fact_base(),
            selection.acquired_native_closures(),
            &mut timing,
        )?
        .close();
        timing.finish(result.is_ok());
        result
    }

    fn certified_output_rows(
        selection: &crate::certified_products::CertifiedSourceSelection,
        projection: &CompilerInputProjection,
        artifacts: &ArtifactView,
    ) -> Result<BTreeMap<ExactModuleIdentity, ResolvedCompilerRole>, CompileError> {
        let mut selected = BTreeMap::new();
        for (owner, role) in projection.snapshot_roles() {
            if let Some(source) = selection.compiler_input_support(owner) {
                let prior = source
                    .selected
                    .get(owner)
                    .ok_or_else(|| failure("compiler continuation lost its actual selected row"))?;
                if prior.interface().0.artifact != role.interface()
                    || role.original().is_some_and(|original| {
                        prior
                            .original()
                            .is_none_or(|(binding, _)| binding.artifact != original)
                    })
                {
                    return Err(failure(
                        "compiler continuation differs from its actual selected row",
                    ));
                }
                let mut row = prior.clone();
                if role.original().is_none() {
                    row.select_type_only();
                }
                selected.insert(owner.clone(), row.with_issued_provenance(role)?);
                continue;
            }
            let canonical = *artifacts
                .lease
                .selection
                .namespace
                .get(&role.interface())
                .ok_or_else(|| failure("completed output lacks its exact canonical binding"))?;
            let row = if let Some(original) = role.original() {
                let mut candidates = Vec::new();
                for offer in selection
                    .issued_native_availability()
                    .flat_map(|row| row.offers())
                {
                    if offer.original() != original
                        || !offer.matches_direct_canonical_binding(
                            original,
                            role.interface(),
                            artifacts,
                        )
                    {
                        continue;
                    }
                    for carrier in offer
                        .artifact_carriers()
                        .iter()
                        .filter(|carrier| carrier.artifact == original)
                    {
                        let state = offer
                            .custody_lease()
                            .inventory
                            .0
                            .lock()
                            .expect("inventory lock");
                        if ResolvedCompilerRole::validate_original_pair(&state, canonical, *carrier)
                            .is_err()
                        {
                            continue;
                        }
                        if !candidates.iter().any(
                            |(prior_carrier, prior): &(ArtifactBinding, &IssuedNativeOffer)| {
                                prior_carrier == carrier && prior.same_issuance(offer)
                            },
                        ) {
                            candidates.push((*carrier, offer));
                        }
                    }
                }
                let [(carrier, offer)] = candidates.as_slice() else {
                    return Err(admission_failure(
                        ArtifactInventoryFailure::BindingAmbiguity { artifact: original },
                    ));
                };
                let interface = offer
                    .custody_lease()
                    .payloads
                    .get(&role.interface())
                    .ok_or_else(|| {
                        failure("completed output offer lost its actual canonical acquisition")
                    })?;
                ResolvedCompilerRole::from_issued_bindings(
                    (canonical, Arc::clone(interface)),
                    Some((*carrier, Arc::clone(offer.original_acquisition()))),
                )
            } else {
                let interface = artifacts
                    .lease
                    .payloads
                    .get(&role.interface())
                    .ok_or_else(|| failure("completed TYPE output lacks its produced interface"))?;
                ResolvedCompilerRole::from_issued_bindings((canonical, Arc::clone(interface)), None)
            };
            selected.insert(owner.clone(), row.with_issued_provenance(role)?);
        }
        Ok(selected)
    }

    /// Join authenticated fragments without asserting that either supplies the
    /// receiving request's complete dependency closure.
    pub(crate) fn merge_output_fragments(&self, other: &Self) -> Result<Self, CompileError> {
        let inputs = ArtifactView::merge_admitted_output_fragments(self, other)?;
        let availability = self
            .native_availability
            .merge(&other.native_availability)?
            .into_rows();
        let mut timing = crate::timing::InvocationTiming::start_scoped(
            "exact.compiler_support.merge_output_fragments",
            "retain_issuance",
        );
        let result = Self::complete_resolved(
            inputs,
            self.native_offers
                .iter()
                .chain(other.native_offers.iter())
                .cloned(),
            availability,
            Some(&self.native_fact_owner),
            self.native_closures.append(&other.native_closures)?,
            &mut timing,
        )?
        .close();
        timing.finish(result.is_ok());
        result
    }

    pub(crate) fn for_output_continuation(
        &self,
        support: &ArtifactView,
        generated: &ExactModuleIdentity,
    ) -> Result<Self, CompileError> {
        let mut selected = self.selected.as_ref().clone();
        let mut projection = self.projection.clone();
        {
            let state = support.lease.inventory.0.lock().expect("inventory lock");
            let owned = &support.read_projection(&state).nodes;
            for (owner, row) in self.selected.iter() {
                let retained = row
                    .selected_bindings()
                    .all(|binding| owned.contains(&InventoryNodeKey::Artifact(binding)));
                if retained {
                    continue;
                }
                if owner != generated {
                    return Err(failure(
                        "private compiler continuation omitted an acquired original",
                    ));
                }
                selected.changed_owners.insert(owner.clone());
                selected.rows.remove(owner);
                projection.roles.remove(owner);
                projection.recorded_roles.remove(owner);
            }
        }
        let input_decisions = self.input_decisions.restricted(&projection, support);
        let mut offers = Vec::new();
        for offer in self.native_offers.iter() {
            if let Some(offer) = offer.within_assembly(projection.role_view(), support)? {
                if !offers
                    .iter()
                    .any(|prior: &IssuedNativeOffer| prior.same_issuance(&offer))
                {
                    offers.push(offer);
                }
            }
        }
        let artifacts = support.retain_issued_offer_custody(offers.iter().cloned())?;
        let availability = self
            .native_availability
            .for_compiler_roles(selected.admitted_roles(), &artifacts)?;
        let mut draft = self.reopen();
        draft.projection = projection;
        draft.selected = Arc::new(selected);
        draft.artifacts = artifacts;
        draft.input_decisions = input_decisions;
        draft.native_availability = availability;
        draft.native_offers = offers.into();
        draft.native_offer_facts = draft
            .native_offers
            .iter()
            .map(|offer| Ok((offer.snapshot_identity()?, offer.snapshot_facts())))
            .collect::<Result<_, CompileError>>()?;
        draft.close()
    }

    /// Select an authored Original only from its already issued offer and exact
    /// carrier edge to the selected canonical row. Ambiguous carriers refuse;
    /// an equal payload elsewhere in custody cannot become the selected input.
    pub(crate) fn promote_issued_authored_originals(
        &self,
        context: &Self,
        owners: &BTreeSet<ExactModuleIdentity>,
    ) -> Result<Self, CompileError> {
        let mut draft = self.reopen();
        let mut selected = self.selected.as_ref().clone();
        for owner in owners {
            let Some(current) = context.compiler_roles().get(owner) else {
                continue;
            };
            let Some(row) = selected.rows.get(owner) else {
                continue;
            };
            let canonical = row.interface().clone();
            let mut candidates = BTreeMap::<ArtifactBinding, Arc<ArtifactEntry>>::new();
            for offer in self.native_offers.iter() {
                let facts = offer.snapshot_facts();
                if &facts.owner != owner
                    || facts.interface != current.interface()
                    || current.original().is_some_and(|id| id != facts.original)
                {
                    continue;
                }
                let state = offer
                    .custody_lease()
                    .inventory
                    .0
                    .lock()
                    .expect("inventory lock");
                for carrier in offer.artifact_carriers() {
                    if carrier.artifact == facts.original
                        && state.outgoing(InventoryNodeKey::Artifact(*carrier)).any(
                            |(target, edge)| {
                                target == InventoryNodeKey::Artifact(canonical.0)
                                    && *edge == ArtifactDependency::Interface
                            },
                        )
                    {
                        let acquisition = offer.original_acquisition();
                        if candidates
                            .get(carrier)
                            .is_some_and(|prior| !prior.same_issuing_authority(acquisition))
                        {
                            return Err(admission_failure(
                                ArtifactInventoryFailure::BindingAmbiguity {
                                    artifact: carrier.artifact,
                                },
                            ));
                        }
                        candidates
                            .entry(*carrier)
                            .or_insert_with(|| Arc::clone(acquisition));
                    }
                }
            }
            let mut candidates = candidates.into_iter();
            let Some((carrier, original)) = candidates.next() else {
                continue;
            };
            if candidates.next().is_some() {
                return Err(admission_failure(
                    ArtifactInventoryFailure::BindingAmbiguity {
                        artifact: carrier.artifact,
                    },
                ));
            }
            let role = CompilerInputRole::ReusableOriginal {
                interface: canonical.0.artifact,
                original: carrier.artifact,
            };
            selected.changed_owners.insert(owner.clone());
            selected.rows.insert(
                owner.clone(),
                ResolvedCompilerRole::from_issued_bindings(canonical, Some((carrier, original))),
            );
            draft.projection.admit_role(owner.clone(), role)?;
        }
        draft.input_decisions = self.input_decisions.for_exact_roles(&draft.projection);
        draft.native_availability = self
            .native_availability
            .for_compiler_roles(selected.admitted_roles(), &draft.artifacts)?;
        draft.selected = Arc::new(selected);
        draft.close()
    }

    /// Reopen a closed support as assembly state for one bounded mutation.
    /// The mutation must return through [`SupportDraft::close`].
    fn reopen(&self) -> SupportDraft {
        SupportDraft {
            projection: self.projection.clone(),
            artifacts: self.artifacts.clone(),
            native_fact_owner: Some(Arc::clone(&self.native_fact_owner)),
            native_offers: Arc::clone(&self.native_offers),
            native_closures: self.native_closures.clone(),
            native_offer_facts: self.native_offer_facts.clone(),
            native_availability: self.native_availability.clone(),
            selected: Arc::clone(&self.selected),
            input_decisions: self.input_decisions.clone(),
        }
    }

    pub(crate) fn native_fact_owner(&self) -> &Arc<crate::exact_context::PublicationNativeFacts> {
        &self.native_fact_owner
    }

    /// Derive a semantic owner from the same acquired cold roles, rows and
    /// exact executable choices. The cold owner is not retained as a parent.
    fn project_demanded_inputs_draft(
        self: &Arc<Self>,
        demand: &DemandCompilerProjection,
        availability: &crate::declaration_context::RequestNativeAvailability,
        issued_delta: &crate::declaration_context::IssuedNativePermissionDelta,
        groups: &crate::declaration_context::DemandCertifiedGroups,
        operation: &tidepool_repr::execution_schema::InventoryOperation,
    ) -> Result<SupportDraft, CompileError> {
        if !Arc::ptr_eq(self, demand.source_support()) || !groups.belongs_to_source(self) {
            return Err(failure(
                "demanded semantic inputs have another acquired source owner",
            ));
        }
        let charge = |result: Result<(), tidepool_repr::execution_schema::ParseError>| {
            result.map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))
        };
        let mut keys = crate::exact_context::PublicationNativeMapKeys {
            roles: BTreeSet::new(),
            offers: BTreeSet::new(),
            native_availability: BTreeSet::new(),
        };
        let mut namespace = BTreeMap::new();
        let mut selected = imbl::OrdMap::new();
        for (owner, role) in demand.projection().snapshot_roles() {
            crate::host_work::checkpoint()?;
            crate::host_work::record_host_work(
                crate::host_work::HostWorkCounter::DemandSemanticRoleVisit,
                1,
            );
            charge(operation.charge(1))?;
            let row = self
                .selected
                .get(owner)
                .ok_or_else(|| failure("demanded semantic role lost its acquired bindings"))?;
            if row.interface().0.artifact != role.interface()
                || row.original().map(|(binding, _)| binding.artifact) != role.original()
            {
                return Err(failure(
                    "demanded semantic role differs from its acquired bindings",
                ));
            }
            charge(
                operation.reserve::<(ExactModuleIdentity, ResolvedCompilerRole, [usize; 4])>(1),
            )?;
            charge(operation.charge(owner.unit.len().saturating_add(owner.module.len())))?;
            selected.insert(owner.clone(), row.clone());
            charge(operation.reserve::<(ExactModuleIdentity, [usize; 4])>(1))?;
            charge(operation.charge(owner.unit.len().saturating_add(owner.module.len())))?;
            keys.roles.insert(owner.clone());
            for (binding, _) in row.bindings() {
                if namespace
                    .get(&binding.artifact)
                    .is_some_and(|prior| prior != binding)
                {
                    return Err(failure(
                        "demanded semantic namespace has competing acquired bindings",
                    ));
                }
                if !namespace.contains_key(&binding.artifact) {
                    charge(operation.reserve::<(ArtifactId, ArtifactBinding, [usize; 4])>(1))?;
                    namespace.insert(binding.artifact, *binding);
                }
            }
        }
        let mut offers = BTreeMap::new();
        for row in availability.rows() {
            crate::host_work::checkpoint()?;
            crate::host_work::record_host_work(
                crate::host_work::HostWorkCounter::DemandNativeAvailabilityVisit,
                1,
            );
            charge(operation.charge(1))?;
            let id = row.original_artifact_id();
            let old = self
                .native_availability
                .issued_row(id)
                .ok_or_else(|| failure("demanded native row has no issued permission"))?;
            if !Arc::ptr_eq(old, row) {
                // A pointer-distinct row with equal permission facts is the
                // same permission. Only a genuinely narrowed or changed row
                // needs its own bounded issuance; reusing the broader cold
                // leaf would change authority.
                let old_facts =
                    crate::declaration_context::RequestNativeAvailability::row_snapshot_facts(old)?;
                let new_facts =
                    crate::declaration_context::RequestNativeAvailability::row_snapshot_facts(row)?;
                let (old_id, new_id) = (old_facts.identity()?, new_facts.identity()?);
                if old_id != new_id && !issued_delta.admits(self, id, &old_facts, &new_facts) {
                    // Rows narrowed by this demand owner are admitted only by
                    // the exact delta it issued for them. A raw narrower row,
                    // another owner's delta or an unissued fact identity is a
                    // typed refusal, never a permission.
                    return Err(CompileError::CompilerEvidence(Box::new(
                        crate::certified_products::CertificationError::ChangedNativePermissionRequiresIssuance {
                            id,
                            old: old_id,
                            new: new_id,
                        },
                    )));
                }
            }
            charge(operation.reserve::<(ArtifactId, [usize; 4])>(1))?;
            keys.native_availability.insert(id);
            for offer in row.offers() {
                let id = offer.snapshot_identity()?;
                if !offers.contains_key(&id) {
                    charge(operation.reserve::<([u8; 32], IssuedNativeOffer, [usize; 4])>(1))?;
                    charge(operation.reserve::<([u8; 32], [usize; 4])>(1))?;
                    keys.offers.insert(id);
                    offers.insert(id, offer.clone());
                }
            }
        }
        // Executable bindings are derived from certified rows during
        // certification, so this loop and `groups()` describe one authority.
        if groups.groups().len() != groups.bound_rows().len() {
            return Err(failure(
                "demanded certified groups lost their executable bindings",
            ));
        }
        for (binding, _) in groups.bound_rows() {
            let key = NativeGroupKey {
                artifact: binding.binding.artifact(),
                original_ordinal: binding.original_ordinal,
            };
            crate::host_work::checkpoint()?;
            crate::host_work::record_host_work(
                crate::host_work::HostWorkCounter::DemandNativeGroupVisit,
                1,
            );
            charge(operation.charge(1))?;
            let row = availability
                .issued_row(key.artifact)
                .ok_or_else(|| failure("demanded semantic group lost its issued row"))?;
            if row
                .dependency_support(key, row.original_owner())
                .is_none_or(|support| !support.groups().any(|candidate| candidate == binding))
            {
                return Err(failure(
                    "demanded semantic group differs from its exact issued choice",
                ));
            }
        }
        let (facts, _) = self
            .native_fact_owner()
            .project(keys, operation)
            .map_err(|error| failure(&format!("demanded native fact projection: {error}")))?;
        let artifacts = self.artifacts.select_acquired_compiler_bindings(
            &namespace,
            groups.bound_rows().map(|(binding, _)| binding),
            operation,
        )?;
        charge(operation.reserve::<IssuedNativeOffer>(offers.len()))?;
        Ok(SupportDraft {
            projection: demand.projection().clone(),
            artifacts,
            native_offers: offers.into_values().collect::<Vec<_>>().into(),
            native_availability: availability.clone(),
            input_decisions: self.input_decisions.with_recorded_facts(
                self.input_decisions
                    .for_exact_roles(demand.projection())
                    .facts,
            ),
            native_offer_facts: facts.offers.clone(),
            native_closures: self.native_closures.clone(),
            native_fact_owner: Some(facts),
            // The home census belongs to the demanded rows, not to cold
            // custody or units merely mentioned by another retained row.
            selected: Arc::new(SelectedCompilerRows::from_admitted(
                selected.into_iter().collect(),
            )),
        })
    }

    pub(crate) fn project_demanded_inputs(
        self: &Arc<Self>,
        demand: &DemandCompilerProjection,
        availability: &crate::declaration_context::RequestNativeAvailability,
        issued_delta: &crate::declaration_context::IssuedNativePermissionDelta,
        groups: &crate::declaration_context::DemandCertifiedGroups,
        operation: &tidepool_repr::execution_schema::InventoryOperation,
    ) -> Result<Arc<Self>, CompileError> {
        Ok(Arc::new(
            self.project_demanded_inputs_draft(
                demand,
                availability,
                issued_delta,
                groups,
                operation,
            )?
            .retain_runtime_custody(&self.artifacts)?
            .close()?,
        ))
    }

    /// Compose independently issued initial native choices with this actual
    /// prefix's compiler projection. The ordinary same-basis projection keeps
    /// its guards; only the checked emitted stage can add historical edges.
    pub(crate) fn project_world_stage_inputs(
        self: &Arc<Self>,
        demand: &DemandCompilerProjection,
        availability: &crate::declaration_context::RequestNativeAvailability,
        issued_delta: &crate::declaration_context::IssuedNativePermissionDelta,
        prefix_groups: &crate::declaration_context::DemandCertifiedGroups,
        stage: &crate::compiler_world::native_demand::CheckedWorldStageNativeInputs,
        original_bindings: &crate::certified_products::OriginalBindingSelection,
        receipt: &crate::certified_products::CertifiedReceipt,
        products: &crate::certified_products::ParsedModuleProducts,
        produced: &crate::checked_cell::ProducedValueTypeInterfaces,
        operation: &Arc<tidepool_repr::execution_schema::InventoryOperation>,
    ) -> Result<Arc<Self>, CompileError> {
        if !Arc::ptr_eq(stage.operation(), operation)
            || !original_bindings.belongs_to_world_stage(stage, operation)
            || !original_bindings.belongs_to(
                &stage.initial_inputs().shared_compiler_source_support(),
                operation,
            )
            || !prefix_groups.groups().is_empty()
        {
            return Err(failure(
                "World prefix projection cannot issue native group demand",
            ));
        }
        let mut projected = self.project_demanded_inputs_draft(
            demand,
            availability,
            issued_delta,
            prefix_groups,
            operation,
        )?;
        projected.artifacts = projected
            .artifacts
            .with_world_stage_initial_inputs(stage, original_bindings)?
            .retain_original_binding_metadata(original_bindings, operation)?;
        // A fresh native body can reuse an already selected TYPE interface.
        // Retain only matching returned owners as issuance candidates; the
        // physical issuer still checks the finalized artifact identity and
        // complete qualified edge row before reusing any binding.
        let charge = |result: Result<(), tidepool_repr::execution_schema::ParseError>| {
            result.map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))
        };
        let mut owners = Vec::new();
        for product in products.products() {
            crate::host_work::checkpoint()?;
            charge(operation.charge(receipt.modules.len()))?;
            let Some(module) = receipt
                .modules
                .iter()
                .find(|module| module.unit == product.unit && module.module == product.module)
            else {
                return Err(failure("World fresh TYPE anchor lacks its receipt owner"));
            };
            if module.origin != crate::certified_products::ProductOrigin::Fresh {
                continue;
            }
            charge(operation.charge(product.unit.len() + product.module.len()))?;
            charge(operation.reserve::<ExactModuleIdentity>(1))?;
            let owner = ExactModuleIdentity {
                unit: product.unit.clone(),
                module: product.module.clone(),
            };
            let Some(row) = self.selected.get(&owner) else {
                continue;
            };
            let ArtifactPayload::Canonical(interface) = &row.interface().1.payload else {
                continue;
            };
            charge(operation.charge(product.interface.len()))?;
            if module.skinny_iface_sha256 != row.interface().1.descriptor.interface_sha256
                || product.interface.as_slice() != interface.interface_bytes()
            {
                continue;
            }
            owners.push(owner);
        }
        // Prior checked outputs may be returned as TYPE metadata without an
        // executable value demand. Keep their actual advanced-prefix bindings;
        // current and future outputs cannot supply an anchor here.
        for output in produced.before_segment().interfaces() {
            crate::host_work::checkpoint()?;
            let expected = output.interface();
            charge(operation.charge(expected.unit().len() + expected.module().len()))?;
            charge(operation.reserve::<ExactModuleIdentity>(1))?;
            let owner = ExactModuleIdentity {
                unit: expected.unit().to_owned(),
                module: expected.module().to_owned(),
            };
            let Some(returned) = receipt
                .finalization
                .value_interfaces
                .get(&(owner.unit.clone(), owner.module.clone()))
            else {
                continue;
            };
            let row = self.selected.get(&owner).ok_or_else(|| {
                failure("World prior produced TYPE metadata lacks its selected owner")
            })?;
            let ArtifactPayload::Interface(interface, JoinedInterfaceRole::ValueInterface) =
                &row.interface().1.payload
            else {
                return Err(failure(
                    "World prior produced TYPE metadata changed its role",
                ));
            };
            charge(operation.charge(
                expected.interface_bytes().len()
                    + expected.package_imports_bytes().len()
                    + output.requirements().len(),
            ))?;
            if output.artifact_id() != row.interface().0.artifact
                || interface.interface_bytes() != expected.interface_bytes()
                || interface.package_imports_bytes() != expected.package_imports_bytes()
                || row.interface().1.requirements.as_slice() != output.requirements()
                || returned.interface.sha256 != row.interface().1.descriptor.interface_sha256
                || returned.interface_requirements.len() != output.requirements().len()
                || returned
                    .interface_requirements
                    .keys()
                    .zip(output.requirements())
                    .any(|((unit, module), required)| {
                        unit != &required.unit || module != &required.module
                    })
            {
                return Err(failure(
                    "World prior produced TYPE metadata differs from its captured owner",
                ));
            }
            owners.push(owner);
        }
        if !owners.is_empty() {
            // TYPE projection drops Original roles, native offers and choices.
            // Its exact canonical dependency rows remain owned by the prefix.
            let anchors = self
                .selected_interfaces()?
                .type_interfaces_for_owners(owners)?;
            charge(
                operation
                    .reserve::<(ArtifactView, ResolvedCompilerRole)>(anchors.selected.rows.len()),
            )?;
            let additions = anchors
                .selected
                .iter()
                .map(|(_, row)| (anchors.artifacts.clone(), row.clone()))
                .collect::<Vec<_>>();
            projected.artifacts = projected.artifacts.append_type_interfaces(&additions)?;
        }
        Ok(Arc::new(
            projected.retain_runtime_custody(&self.artifacts)?.close()?,
        ))
    }

    #[tracing::instrument(
        target = "exomonad_harness::timing", name = "exact.compiler_support.new",
        level = "debug", skip_all, fields(inclusive = true, role_count = projection.roles.len())
    )]
    fn new(
        projection: CompilerInputProjection,
        artifacts: ArtifactView,
        native_offers: impl IntoIterator<Item = IssuedNativeOffer>,
        native_availability: crate::declaration_context::RequestNativeAvailability,
    ) -> Result<Self, CompileError> {
        projection.validate(&artifacts)?;
        if projection
            .roles
            .values()
            .any(CompilerInputRole::is_published_source_original)
        {
            return Err(failure(
                "raw publication facts require recorded authentication",
            ));
        }
        let selected = projection.resolve_associations(&artifacts)?;
        Self::from_authenticated_rows(
            projection,
            artifacts,
            selected,
            native_offers,
            native_availability,
            IssuedInputNativeDecisions::default(),
            None,
            AcquiredNativeClosures::default(),
        )
    }

    fn from_authenticated_rows(
        projection: CompilerInputProjection,
        artifacts: ArtifactView,
        selected: BTreeMap<ExactModuleIdentity, ResolvedCompilerRole>,
        native_offers: impl IntoIterator<Item = IssuedNativeOffer>,
        native_availability: crate::declaration_context::RequestNativeAvailability,
        input_decisions: IssuedInputNativeDecisions,
        prior: Option<&Arc<crate::exact_context::PublicationNativeFacts>>,
        native_closures: AcquiredNativeClosures,
    ) -> Result<Self, CompileError> {
        let mut timing = crate::timing::InvocationTiming::start_scoped(
            "exact.compiler_support.new",
            "validate_projection",
        );
        timing.advance("retain_selected_rows");
        let support = Self::complete_resolved(
            ResolvedCompilerInputs {
                artifacts,
                projection,
                selected,
                input_decisions,
            },
            native_offers,
            native_availability,
            prior,
            native_closures,
            &mut timing,
        )?;
        timing.finish(true);
        support.close()
    }

    fn complete_resolved(
        inputs: ResolvedCompilerInputs,
        native_offers: impl IntoIterator<Item = IssuedNativeOffer>,
        native_availability: crate::declaration_context::RequestNativeAvailability,
        prior: Option<&Arc<crate::exact_context::PublicationNativeFacts>>,
        native_closures: AcquiredNativeClosures,
        timing: &mut crate::timing::InvocationTiming,
    ) -> Result<SupportDraft, CompileError> {
        let ResolvedCompilerInputs {
            artifacts,
            projection,
            selected,
            input_decisions: _,
        } = inputs;
        let artifacts = native_closures
            .iter()
            .try_fold(artifacts, |view, completion| {
                view.retain_custody(completion.artifacts())
            })?;
        timing.advance("retain_issued_offer_custody");
        let mut offers = Vec::new();
        let mut native_offer_facts = prior.map(|p| p.offers.clone()).unwrap_or_default();
        let mut kept_prior = 0usize;
        let mut fact_error = None;
        let retained = artifacts.retain_issued_offer_custody(
            native_offers.into_iter().filter_map(|offer| {
                if !offers
                    .iter()
                    .any(|prior: &IssuedNativeOffer| prior.same_issuance(&offer))
                {
                    match offer.snapshot_identity() {
                        Ok(identity) => {
                            if prior.is_some_and(|prior| prior.offers.contains_key(&identity)) {
                                kept_prior += 1;
                            } else {
                                native_offer_facts.insert(identity, offer.snapshot_facts());
                            }
                        }
                        Err(error) => {
                            fact_error = Some(error);
                            return None;
                        }
                    }
                    offers.push(offer.clone());
                    Some(offer)
                } else {
                    None
                }
            }),
        )?;
        if let Some(error) = fact_error {
            return Err(error);
        }
        if prior.is_some_and(|prior| kept_prior != prior.offers.len()) {
            // A genuine narrowing removes facts; unchanged/repeated offer owners
            // retain the persistent root without rebuilding their facts.
            let kept = offers
                .iter()
                .map(IssuedNativeOffer::snapshot_identity)
                .collect::<Result<BTreeSet<_>, _>>()?;
            let removed = native_offer_facts
                .keys()
                .filter(|id| !kept.contains(*id))
                .copied()
                .collect::<Vec<_>>();
            for id in removed {
                native_offer_facts.remove(&id);
            }
        }
        // Custody-only retention preserves these exact selected bindings while
        // it may acquire additional immutable sources for their payloads.
        timing.advance("qualify_canonical_availability");
        let selected = Arc::new(SelectedCompilerRows::from_admitted(selected));
        let input_decisions = selected.decision_facts();
        let native_availability =
            native_availability.for_compiler_roles(selected.admitted_roles(), &retained)?;
        timing.advance("assemble_support");
        let input_decisions = input_decisions.with_recorded_facts(input_decisions.facts.clone());
        let support = SupportDraft {
            native_availability,
            projection,
            artifacts: retained,
            native_fact_owner: prior.cloned(),
            native_offer_facts,
            native_closures,
            native_offers: offers.into(),
            selected,
            input_decisions,
        };
        Ok(support)
    }

    fn append_produced_interfaces(
        &self,
        projection: CompilerInputProjection,
        artifacts: ArtifactView,
        interfaces: impl IntoIterator<Item = Arc<ArtifactEntry>>,
    ) -> Result<Self, CompileError> {
        let mut selected = self.selected.as_ref().clone();
        for entry in interfaces {
            let owner = &entry.descriptor.owner;
            let role = projection
                .roles
                .get(owner)
                .ok_or_else(|| failure("produced interface lacks its selected role"))?;
            if role.interface() != entry.descriptor.id {
                return Err(failure(
                    "produced interface differs from selected compiler role",
                ));
            }
            if selected.rows.contains_key(owner) {
                continue;
            }
            if role.original().is_some() {
                return Err(failure("produced interface cannot issue an Original role"));
            }
            let binding = *artifacts
                .lease
                .selection
                .namespace
                .get(&entry.descriptor.id)
                .ok_or_else(|| failure("produced interface lacks its exact binding"))?;
            selected.changed_owners.insert(owner.clone());
            selected.rows.insert(
                owner.clone(),
                ResolvedCompilerRole::from_issued_bindings((binding, entry), None),
            );
        }
        let mut draft = self.reopen();
        draft.projection = projection;
        draft.artifacts = artifacts;
        draft.selected = Arc::new(selected);
        draft.close()
    }

    fn rebuild(
        &self,
        projection: CompilerInputProjection,
        artifacts: ArtifactView,
        offers: impl IntoIterator<Item = IssuedNativeOffer>,
    ) -> Result<Self, CompileError> {
        self.rebuild_narrowed(projection, artifacts, offers)
    }

    fn rebuild_narrowed(
        &self,
        projection: CompilerInputProjection,
        artifacts: ArtifactView,
        offers: impl IntoIterator<Item = IssuedNativeOffer>,
    ) -> Result<Self, CompileError> {
        let decisions = self.input_decisions.restricted(&projection, &artifacts);
        let selected = projection
            .roles
            .iter()
            .map(|(owner, role)| {
                let mut row =
                    self.selected.get(owner).cloned().ok_or_else(|| {
                        failure("narrowed compiler role lacks its acquired owner")
                    })?;
                if row.interface().0.artifact != role.interface() {
                    return Err(failure("narrowing changed the acquired canonical binding"));
                }
                match (role.original(), row.original()) {
                    (None, _) => row.select_type_only(),
                    (Some(id), Some((binding, _))) if id == binding.artifact => {}
                    _ => return Err(failure("narrowing changed the acquired original binding")),
                }
                row.narrow_native_choice(&artifacts);
                let row = row.with_issued_provenance(role)?;
                Ok((owner.clone(), row))
            })
            .collect::<Result<BTreeMap<_, _>, CompileError>>()?;
        {
            let state = artifacts.lease.inventory.0.lock().expect("inventory lock");
            let owned = &artifacts.read_projection(&state).nodes;
            for row in selected.values() {
                for (binding, _) in row.bindings() {
                    if artifacts
                        .lease
                        .selection
                        .namespace
                        .get(&binding.artifact)
                        .is_some_and(|selected| selected != binding)
                        || !owned.contains(&InventoryNodeKey::Artifact(*binding))
                    {
                        return Err(failure("narrowing changed an acquired namespace binding"));
                    }
                }
            }
        }
        projection.validate_selected_requirements(&selected)?;
        let selected = SelectedCompilerRows::from_admitted(selected);
        let offers = offers
            .into_iter()
            .filter_map(
                |offer| match offer.is_relevant_to_support(selected.admitted_roles()) {
                    Ok(true) => Some(Ok(offer)),
                    Ok(false) => None,
                    Err(error) => Some(Err(error)),
                },
            )
            .collect::<Result<Vec<_>, _>>()?;
        let mut timing = crate::timing::InvocationTiming::start_scoped(
            "exact.compiler_support.narrow",
            "retain_acquired_rows",
        );
        Self::complete_resolved(
            ResolvedCompilerInputs {
                projection,
                artifacts,
                selected: selected.rows.into_iter().collect(),
                input_decisions: decisions,
            },
            offers,
            self.native_availability.clone(),
            Some(&self.native_fact_owner),
            self.native_closures.clone(),
            &mut timing,
        )?
        .close()
    }

    fn resolve_input_policy(&self) -> Result<Self, CompileError> {
        if self.selected.values().all(|row| {
            row.original().is_none()
                || matches!(row.native_decision(), InputNativeDecision::IssuedChoices(_))
        }) {
            return Ok(self.clone());
        }
        let (artifacts, projection, decisions) = self.artifacts.merge_compiler_inputs_with_demand(
            &self.projection,
            &self.artifacts,
            &CompilerInputProjection::default(),
            CompilerNativeDemand::SourceSelections {
                left: Some(&self.input_decisions),
                right: None,
            },
            CompilerInputValidation::ReceivedFragment,
        )?;
        let mut selected = self.selected.as_ref().clone();
        for (owner, fact) in decisions.facts {
            let Some(row) = selected
                .rows
                .get(&owner)
                .filter(|row| matches!(row.native_decision(), InputNativeDecision::UnissuedPolicy))
            else {
                continue;
            };
            let original = row.original().expect("input decision Original");
            for (binding, _) in row.bindings() {
                if artifacts.lease.selection.namespace.get(&binding.artifact) != Some(binding) {
                    return Err(failure(
                        "input policy changed its selected compiler binding",
                    ));
                }
            }
            let row = row.clone().with_native_choice(fact.groups, |group| {
                let mut offers = self.native_offers.iter().filter(|offer| {
                    offer
                        .original_acquisition()
                        .same_issuing_authority(&original.1)
                        && offer.contains_checked_group(group)
                });
                match (offers.next(), offers.next()) {
                    (Some(offer), None) => {
                        SelectedNativeSupport::Acquired(Arc::from([offer.clone()]))
                    }
                    _ => SelectedNativeSupport::missing(),
                }
            });
            selected.changed_owners.insert(owner.clone());
            selected.rows.insert(owner, row);
        }
        let mut draft = self.reopen();
        draft.projection = projection;
        draft.artifacts = artifacts;
        draft.selected = Arc::new(selected);
        draft.close()
    }

    /// Issue availability at a checked original input ingress from its exact
    /// selected graph. Custody outside that selection cannot supply bodies.
    fn from_selected_issuance(
        projection: CompilerInputProjection,
        artifacts: ArtifactView,
    ) -> Result<Self, CompileError> {
        projection.validate(&artifacts)?;
        let mut selected = projection.resolve_associations(&artifacts)?;
        let mut offers = Vec::new();
        let mut input_decisions = IssuedInputNativeDecisions::default();
        for (owner, role) in projection.snapshot_roles() {
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
                let input = CompilerPrimaryInput::Original(CompilerPrimaryRow {
                    owner,
                    row: &selected[owner],
                    artifacts: &artifacts,
                });
                let offer = IssuedNativeOffer::issue(input, proof)?;
                let row = selected.remove(owner).expect("issued compiler row");
                selected.insert(
                    owner.clone(),
                    row.with_native_choice(groups.clone(), |_| {
                        SelectedNativeSupport::Acquired(Arc::from([offer.clone()]))
                    }),
                );
                input_decisions.facts.insert(
                    owner.clone(),
                    RecordedInputNativeDecision {
                        role: role.recorded_fact(),
                        groups,
                    },
                );
                offers.push(offer);
            }
        }
        let metadata = CompilerArtifactMetadata {
            entries: selected
                .iter()
                .map(|(owner, row)| {
                    (
                        owner.clone(),
                        Arc::clone(&row.original().unwrap_or(row.interface()).1),
                    )
                })
                .collect(),
            retained: artifacts.metadata_snapshot(),
        };
        let mut timing = crate::timing::InvocationTiming::start_scoped(
            "exact.compiler_support.issue",
            "retain_issuance",
        );
        let mut draft = Self::complete_resolved(
            ResolvedCompilerInputs {
                artifacts,
                projection,
                selected,
                input_decisions,
            },
            offers,
            crate::declaration_context::RequestNativeAvailability::default(),
            None,
            AcquiredNativeClosures::default(),
            &mut timing,
        )?;
        draft.native_availability =
            crate::declaration_context::RequestNativeAvailability::issue_checked(
                draft.selected.admitted_roles(),
                &metadata,
                &draft.artifacts,
                &draft.native_offers,
            )?;
        let result = draft.close();
        timing.finish(result.is_ok());
        result
    }

    pub(crate) fn issue_native_offer(
        &self,
        owner: &ExactModuleIdentity,
        proof: Option<NativeOriginalRequirement>,
    ) -> Result<IssuedNativeOffer, CompileError> {
        let input = self.compiler_primary_input(owner)?.ok_or_else(|| {
            admission_failure(ArtifactInventoryFailure::MissingOriginal {
                owner: owner.clone(),
            })
        })?;
        IssuedNativeOffer::issue(input, proof)
    }

    pub(crate) fn compiler_primary_inputs(
        &self,
    ) -> impl Iterator<Item = Result<CompilerPrimaryInput<'_>, CompileError>> {
        self.selected
            .iter()
            .map(|(owner, row)| self.issue_compiler_primary_input(owner, row))
    }

    pub(crate) fn compiler_primary_input(
        &self,
        owner: &ExactModuleIdentity,
    ) -> Result<Option<CompilerPrimaryInput<'_>>, CompileError> {
        self.selected
            .rows
            .get_key_value(owner)
            .map(|(owner, row)| self.issue_compiler_primary_input(owner, row))
            .transpose()
    }

    fn issue_compiler_primary_input<'a>(
        &'a self,
        owner: &'a ExactModuleIdentity,
        row: &'a ResolvedCompilerRole,
    ) -> Result<CompilerPrimaryInput<'a>, CompileError> {
        for (binding, entry) in row.bindings() {
            if &entry.descriptor.owner != owner
                || self
                    .artifacts
                    .lease
                    .selection
                    .namespace
                    .get(&binding.artifact)
                    != Some(binding)
                || self
                    .artifacts
                    .lease
                    .selection
                    .scopes
                    .get(&binding.selection)
                    .is_none_or(|proof| {
                        proof
                            .proof()
                            .nodes
                            .binary_search(&ArtifactBindingNode::Artifact(*binding))
                            .is_err()
                    })
            {
                return Err(failure(
                    "compiler primary row differs from its acquired owner",
                ));
            }
        }
        if let Some((original, _)) = row.original() {
            let state = self
                .artifacts
                .lease
                .inventory
                .0
                .lock()
                .expect("inventory lock");
            ResolvedCompilerRole::validate_original_pair(&state, row.interface().0, *original)?;
            Ok(CompilerPrimaryInput::Original(CompilerPrimaryRow {
                artifacts: &self.artifacts,
                owner,
                row,
            }))
        } else {
            Ok(CompilerPrimaryInput::InterfaceOnly(CompilerPrimaryRow {
                artifacts: &self.artifacts,
                owner,
                row,
            }))
        }
    }

    pub(crate) fn world_primary_type_row(
        &self,
        owner: &ExactModuleIdentity,
    ) -> Option<WorldPrimaryTypeRow<'_>> {
        #[cfg(test)]
        type_query_work::lookup();
        let (owner, row) = self.selected.rows.get_key_value(owner)?;
        let transport = row.original().unwrap_or(row.interface());
        Some(WorldPrimaryTypeRow {
            owner,
            interface_binding: &row.interface().0,
            original_binding: row.original().map(|(binding, _)| binding),
            transport_binding: &transport.0,
            entry: &transport.1,
        })
    }

    /// Add only already-admitted type roles. Existing roles retain their exact
    /// canonical/native binding pairs and all executable choices remain shared.
    pub(crate) fn with_type_interfaces(
        &self,
        incoming: &[&Self],
        producer: [u8; 32],
    ) -> Result<(Self, Vec<ExactModuleIdentity>), CompileError> {
        let mut projection = self.projection.clone();
        let mut selected = self.selected.as_ref().clone();
        let mut home_units = self.selected.home_units.as_ref().clone();
        let mut additions = BTreeMap::new();
        for support in incoming {
            for (owner, row) in support.selected.iter() {
                crate::host_work::checkpoint()?;
                if producer == [0; 32] || row.interface().1.descriptor.producer_sha256 != producer {
                    return Err(failure(
                        "type-interface row differs from the primary compiler producer",
                    ));
                }
                let role = &support.projection.roles[owner];
                if !matches!(role, CompilerInputRole::InterfaceOnly { .. })
                    || row.original().is_some()
                {
                    return Err(failure("type-interface delta contains an original role"));
                }
                CompilerInputProjection::validate_role_payload(
                    owner,
                    role,
                    &row.interface().1,
                    |_| None,
                )?;
                if let Some(prior) = selected.get(owner) {
                    if prior.interface().0.artifact != row.interface().0.artifact {
                        return Err(admission_failure(ArtifactInventoryFailure::OwnerConflict {
                            owner: owner.clone(),
                        }));
                    }
                    if prior.interface().0 != row.interface().0 {
                        return Err(admission_failure(
                            ArtifactInventoryFailure::BindingAmbiguity {
                                artifact: row.interface().0.artifact,
                            },
                        ));
                    }
                    continue;
                }
                projection.admit_role(owner.clone(), role.clone())?;
                if let ArtifactPayload::Canonical(interface) = &row.interface().1.payload {
                    home_units.extend(interface.home_units().iter().cloned());
                }
                selected.changed_owners.insert(owner.clone());
                selected.rows.insert(owner.clone(), row.clone());
                additions.insert(owner.clone(), (support.artifacts.clone(), row.clone()));
            }
        }
        if additions.is_empty() {
            return Ok((self.clone(), Vec::new()));
        }
        // Incoming contexts already own closed canonical selections. Check only
        // those obligations against the exact combined rows; never census the
        // receiver's untouched roles or executable dependency graph.
        for support in incoming {
            projection.validate_requirements(
                support
                    .selected
                    .iter()
                    .map(|(_, row)| row.interface().1.as_ref()),
                |owner| selected.get(owner).map(|row| row.interface().1.as_ref()),
            )?;
        }
        selected.home_units = Arc::new(home_units);
        let artifacts = self.artifacts.append_type_interfaces(additions.values())?;
        let changed = additions.into_keys().collect();
        let mut draft = self.reopen();
        draft.projection = projection;
        draft.selected = Arc::new(selected);
        draft.artifacts = artifacts;
        Ok((draft.close()?, changed))
    }

    pub(crate) fn same_interface_binding(
        &self,
        issued: &Self,
        owner: &ExactModuleIdentity,
        interface: ArtifactId,
    ) -> bool {
        match (self.selected.get(owner), issued.selected.get(owner)) {
            (Some(actual), Some(expected)) => {
                actual.interface().0 == expected.interface().0
                    && actual
                        .interface()
                        .1
                        .same_issuing_authority(&expected.interface().1)
                    && actual.interface().0.artifact == interface
            }
            _ => false,
        }
    }

    pub(crate) fn implementation_entry(
        &self,
        owner: &ExactModuleIdentity,
    ) -> Option<&Arc<ArtifactEntry>> {
        self.selected.get(owner).map(|row| {
            row.original()
                .map_or(&row.interface().1, |(_, entry)| entry)
        })
    }

    /// Add the actual closed support fragment issued for one program prefix.
    /// Unchanged roles and native facts share their persistent owners. Incoming
    /// offers retain custody without choosing additional executable roots.
    #[track_caller]
    pub(crate) fn append_program_fragment(
        &self,
        incoming: &Self,
    ) -> Result<ProgramSupportDelta, CompileError> {
        let mut projection = self.projection.clone();
        let mut selected = self.selected.as_ref().clone();
        let mut home_units = self.selected.home_units.as_ref().clone();
        let mut changed = BTreeSet::new();
        let mut promotion = false;
        for (owner, row) in incoming.selected.iter() {
            crate::host_work::checkpoint()?;
            let role = &incoming.projection.roles[owner];
            if let Some(previous) = selected.get(owner) {
                promotion |= previous.original().is_none() && row.original().is_some();
                if previous.interface().0.artifact != row.interface().0.artifact {
                    return Err(admission_failure(ArtifactInventoryFailure::OwnerConflict {
                        owner: owner.clone(),
                    }));
                }
                if previous.interface().0 != row.interface().0 {
                    tracing::debug!(
                        target: "tidepool::artifact_inventory::ambiguity_trace",
                        ?owner,
                        previous_binding = ?previous.interface().0,
                        incoming_binding = ?row.interface().0,
                        previous_role = ?self.projection.roles[owner],
                        incoming_role = ?role,
                        receiving_caller = %std::panic::Location::caller(),
                        "program fragment carries different issued interface bindings"
                    );
                    return Err(admission_failure(
                        ArtifactInventoryFailure::BindingAmbiguity {
                            artifact: row.interface().0.artifact,
                        },
                    ));
                }
                if let (Some((old, _)), Some((new, _))) = (previous.original(), row.original()) {
                    if old != new {
                        return Err(admission_failure(
                            ArtifactInventoryFailure::BindingAmbiguity {
                                artifact: new.artifact,
                            },
                        ));
                    }
                }
            }
            let issuer = match projection.roles.get(owner) {
                Some(previous) => CompilerInputProjection::choose_role(owner, previous, role)?,
                None => CompilerRoleIssuer::Incoming,
            };
            if matches!(issuer, CompilerRoleIssuer::Receiving) {
                continue;
            }
            projection.retain_role_selection(&SelectedCompilerRole {
                owner,
                role,
                issuer,
            });
            if let ArtifactPayload::Canonical(interface) = &row.interface().1.payload {
                home_units.extend(interface.home_units().iter().cloned());
            }
            selected.changed_owners.insert(owner.clone());
            selected.rows.insert(owner.clone(), row.clone());
            changed.insert(owner.clone());
        }
        projection.validate_requirements(
            incoming
                .selected
                .iter()
                .flat_map(|(_, row)| row.bindings().map(|(_, entry)| entry.as_ref())),
            |owner| selected.get(owner).map(|row| row.interface().1.as_ref()),
        )?;
        let mut offers = self.native_offers.to_vec();
        let mut native_offer_facts = self.native_offer_facts.clone();
        let mut new_offers = Vec::new();
        for offer in incoming.native_offers.iter() {
            let id = offer.snapshot_identity()?;
            if let Some(previous) = native_offer_facts.get(&id) {
                if previous != &offer.snapshot_facts() {
                    return Err(failure("program support changed an issued native offer"));
                }
            } else {
                native_offer_facts.insert(id, offer.snapshot_facts());
            }
            if !offers.iter().any(|prior| prior.same_issuance(offer)) {
                new_offers.push(offer.clone());
                offers.push(offer.clone());
            }
        }
        let artifacts = self.artifacts.append_admitted_custody(
            incoming.selected.iter().flat_map(|(_, row)| {
                row.bindings()
                    .map(|(binding, entry)| (incoming.artifacts.lease.as_ref(), *binding, entry))
            }),
            &new_offers,
        )?;
        // The draft invariant owns completion custody in its artifacts before
        // the draft exists; self's completions stay retained through the
        // append, and incoming completions merge here exactly once.
        let artifacts = incoming
            .native_closures
            .iter()
            .try_fold(artifacts, |view, completion| {
                view.retain_custody(completion.artifacts())
            })?;
        let input_decisions = selected.decision_facts();
        let mut native_availability = match self
            .native_availability
            .merge(&incoming.native_availability)?
        {
            crate::declaration_context::NativeAvailabilityMerge::Unchanged(rows)
            | crate::declaration_context::NativeAvailabilityMerge::Changed(rows) => rows,
        };
        if promotion {
            // Only this semantic transition can change qualification of prior
            // optional Full rows. The existing qualifier preserves exact
            // historical dependencies independently of direct root choices.
            native_availability =
                native_availability.for_compiler_roles(selected.admitted_roles(), &artifacts)?;
        }
        selected.home_units = Arc::new(home_units);
        Ok(ProgramSupportDelta {
            support: SupportDraft {
                projection,
                artifacts,
                native_fact_owner: Some(Arc::clone(&self.native_fact_owner)),
                native_offers: offers.into(),
                native_offer_facts,
                native_closures: self.native_closures.append(&incoming.native_closures)?,
                native_availability,
                selected: Arc::new(selected),
                input_decisions,
            }
            .close()?,
            changed_owners: changed.into_iter().collect(),
            promotion_qualifier_invocations: usize::from(promotion),
        })
    }

    /// Maintain the catalog representatives and flattened dependency facts for
    /// the exact bindings just appended. Compiler role selection remains the
    /// independent `entries` map below; representatives never grant a role.
    pub(crate) fn program_fragment_metadata(
        &self,
        prior: &CompilerArtifactMetadata,
        incoming: &Self,
        changed_owners: &[ExactModuleIdentity],
    ) -> Result<CompilerArtifactMetadata, CompileError> {
        let mut entries = prior.entries.clone();
        for owner in changed_owners {
            let row = self
                .selected
                .get(owner)
                .ok_or_else(|| failure("program metadata lacks its changed role"))?;
            entries.insert(
                owner.clone(),
                Arc::clone(&row.original().unwrap_or(row.interface()).1),
            );
        }
        let mut retained = prior.retained.as_ref().clone();
        let mut bindings = BTreeSet::new();
        for (_, row) in incoming.selected.iter() {
            bindings.insert(row.interface().0);
            bindings.extend(row.original().map(|(binding, _)| *binding));
        }
        for offer in incoming.native_offers.iter() {
            bindings.extend(offer.artifact_carriers());
        }
        let mut touched = BTreeSet::new();
        for binding in bindings {
            let entry = self
                .artifacts
                .lease
                .payloads
                .get(&binding.artifact)
                .ok_or_else(|| failure("program metadata lacks its acquired payload"))?;
            if touched.insert(binding.artifact) {
                let owner = &entry.descriptor.owner;
                let new_payload = !retained.artifacts.contains_key(&binding.artifact);
                retained
                    .artifacts
                    .insert(binding.artifact, Arc::clone(entry));
                if entry.is_compiler_interface() {
                    if retained
                        .interface_representatives
                        .get(owner)
                        .is_none_or(|prior| prior.descriptor.id <= binding.artifact)
                    {
                        retained
                            .interface_representatives
                            .insert(owner.clone(), Arc::clone(entry));
                    }
                    if retained
                        .entries
                        .get(owner)
                        .is_none_or(|prior| !prior.is_native())
                    {
                        let representative = &retained.interface_representatives[owner];
                        retained
                            .entries
                            .insert(owner.clone(), Arc::clone(representative));
                    }
                } else if entry.is_native() && new_payload {
                    if retained.ambiguous_native_owners.contains(owner) {
                        // The prior exact historical variants remain custody.
                    } else if retained
                        .entries
                        .get(owner)
                        .is_some_and(|prior| prior.is_native())
                    {
                        Arc::make_mut(&mut retained.ambiguous_native_owners).insert(owner.clone());
                        match retained.interface_representatives.get(owner) {
                            Some(interface) => {
                                retained
                                    .entries
                                    .insert(owner.clone(), Arc::clone(interface));
                            }
                            None => {
                                retained.entries.remove(owner);
                            }
                        }
                    } else {
                        retained.entries.insert(owner.clone(), Arc::clone(entry));
                    }
                } else if retained
                    .entries
                    .get(owner)
                    .is_some_and(|prior| prior.descriptor.id == binding.artifact)
                {
                    retained.entries.insert(owner.clone(), Arc::clone(entry));
                }
            }
            let node = InventoryNodeKey::Artifact(binding);
            let proof = self
                .artifacts
                .lease
                .selection
                .scopes
                .get(&binding.selection)
                .ok_or_else(|| failure("program metadata lacks its issuing proof"))?
                .proof();
            let start = proof
                .dependencies
                .partition_point(|(source, _, _)| *source < node);
            for (_, target, dependency) in proof.dependencies[start..]
                .iter()
                .take_while(|(source, _, _)| *source == node)
            {
                retained.dependencies.insert((
                    binding.artifact,
                    target.artifact(),
                    dependency.clone(),
                ));
            }
        }
        Ok(CompilerArtifactMetadata {
            entries,
            retained: Arc::new(retained),
        })
    }

    pub(crate) fn type_interface_metadata(
        &self,
        prior: &CompilerArtifactMetadata,
        changed: &[ExactModuleIdentity],
    ) -> Result<CompilerArtifactMetadata, CompileError> {
        let mut entries = prior.entries.clone();
        let mut retained = prior.retained.as_ref().clone();
        for owner in changed {
            let row = self.selected.get(owner).expect("newly admitted type row");
            let (binding, entry) = row.interface();
            entries.insert(owner.clone(), Arc::clone(entry));
            retained
                .artifacts
                .insert(binding.artifact, Arc::clone(entry));
            if retained
                .interface_representatives
                .get(owner)
                .is_none_or(|prior| prior.descriptor.id < binding.artifact)
            {
                retained
                    .interface_representatives
                    .insert(owner.clone(), Arc::clone(entry));
            }
            // The retained catalog uses the greatest canonical ID, unless a
            // unique native carrier already owns the representative position.
            if retained
                .entries
                .get(owner)
                .is_none_or(|prior| !prior.is_native() && prior.descriptor.id < binding.artifact)
            {
                retained.entries.insert(owner.clone(), Arc::clone(entry));
            }
            let node = InventoryNodeKey::Artifact(*binding);
            let proof = self.artifacts.lease.selection.scopes[&binding.selection].proof();
            let start = proof
                .dependencies
                .partition_point(|(source, _, _)| *source < node);
            for (_, target, dependency) in proof.dependencies[start..]
                .iter()
                .take_while(|(source, _, _)| *source == node)
            {
                retained.dependencies.insert((
                    binding.artifact,
                    target.artifact(),
                    dependency.clone(),
                ));
            }
        }
        Ok(CompilerArtifactMetadata {
            entries,
            retained: Arc::new(retained),
        })
    }

    /// Observe issued canonical interfaces while preserving compiler roles and
    /// native authority in the original support.
    pub(crate) fn selected_interfaces(
        &self,
    ) -> Result<SelectedCompilerInterfaces<'_>, CompileError> {
        Ok(SelectedCompilerInterfaces {
            artifacts: &self.artifacts,
            selected: &self.selected,
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
        use crate::certified_products::{
            ArtifactIdentityMismatch, ArtifactIdentityStage, InputDecisionRefusal,
        };
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
        let admitted = self.selected.get(&owner).ok_or_else(|| {
            InputDecisionRefusal::ArtifactIdentityMismatch(ArtifactIdentityMismatch {
                expected_original: Some(original),
                expected_canonical: Some(canonical),
                ..ArtifactIdentityMismatch::refused(ArtifactIdentityStage::MissingSelectedRow)
            })
        })?;
        let (_, entry) = admitted.original().ok_or_else(|| {
            InputDecisionRefusal::ArtifactIdentityMismatch(ArtifactIdentityMismatch {
                expected_original: Some(original),
                expected_canonical: Some(canonical),
                ..ArtifactIdentityMismatch::refused(ArtifactIdentityStage::MissingOriginalSlot)
            })
        })?;
        if selected_original != original
            || entry.descriptor.id != original
            || role.interface() != canonical
            || admitted.interface().1.descriptor.id != canonical
        {
            return Err(InputDecisionRefusal::ArtifactIdentityMismatch(
                ArtifactIdentityMismatch {
                    stage: ArtifactIdentityStage::RequestedIdentityDiffers,
                    expected_original: Some(original),
                    expected_canonical: Some(canonical),
                    observed_original: Some(entry.descriptor.id),
                    observed_canonical: Some(admitted.interface().1.descriptor.id),
                    observed_kind: None,
                    ..ArtifactIdentityMismatch::refused(
                        ArtifactIdentityStage::RequestedIdentityDiffers,
                    )
                },
            ));
        }
        let ArtifactPayload::Original(product) = &entry.payload else {
            return Err(InputDecisionRefusal::ArtifactIdentityMismatch(
                ArtifactIdentityMismatch {
                    stage: ArtifactIdentityStage::NonOriginalPayload,
                    expected_original: Some(original),
                    expected_canonical: Some(canonical),
                    observed_original: Some(entry.descriptor.id),
                    observed_canonical: Some(admitted.interface().1.descriptor.id),
                    observed_kind: Some(entry.payload.artifact_kind()),
                    ..ArtifactIdentityMismatch::refused(ArtifactIdentityStage::NonOriginalPayload)
                },
            ));
        };
        if product.owner() != native_owner {
            return Err(InputDecisionRefusal::NativeOwnerMismatch);
        }
        match admitted.native_decision() {
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

    #[tracing::instrument(
        target = "exomonad_harness::timing",
        name = "exact.template_availability",
        level = "debug",
        skip_all,
        fields(inclusive = true)
    )]
    pub(crate) fn retain_template_availability(
        &self,
        original: &Self,
    ) -> Result<Self, CompileError> {
        let mut timing = crate::timing::InvocationTiming::start_scoped(
            "exact.template_availability",
            "qualify_original_availability",
        );
        let availability = original
            .native_availability
            .for_compiler_roles(self.compiler_roles(), &original.artifacts)?;
        timing.advance("collect_offers");
        let mut offers = Vec::new();
        for row in availability.rows() {
            for offer in row.offers() {
                if !offers
                    .iter()
                    .any(|prior: &IssuedNativeOffer| prior.same_issuance(offer))
                {
                    offers.push(offer.clone());
                }
            }
        }
        timing.advance("retain_offer_custody");
        let retained = self
            .artifacts
            .retain_issued_offer_custody(offers.iter().cloned())?;
        timing.advance("construct_support");
        let mut draft = self.reopen();
        draft.artifacts = retained;
        draft.native_availability = availability;
        draft.native_offers = offers.into();
        draft.native_offer_facts = draft
            .native_offers
            .iter()
            .map(|offer| Ok((offer.snapshot_identity()?, offer.snapshot_facts())))
            .collect::<Result<_, CompileError>>()?;
        let result = draft.close();
        timing.finish(result.is_ok());
        result
    }

    pub(crate) fn native_availability(
        &self,
    ) -> &crate::declaration_context::RequestNativeAvailability {
        &self.native_availability
    }

    pub(crate) fn retain_normalized_availability(
        &self,
        incoming: &crate::declaration_context::RequestNativeAvailability,
    ) -> Result<Self, CompileError> {
        match self.native_availability.merge(incoming)? {
            // The receiver keeps its exact roles, namespace and prior row owners.
            // An equal incoming row does not transfer additional custody here.
            crate::declaration_context::NativeAvailabilityMerge::Unchanged(_) => Ok(self.clone()),
            crate::declaration_context::NativeAvailabilityMerge::Changed(rows) => {
                let mut draft = self.reopen();
                draft.native_availability =
                    rows.for_compiler_roles(self.compiler_roles(), &self.artifacts)?;
                draft.close()
            }
        }
    }

    fn restore_native_code_records(
        &self,
        snapshot: &crate::exact_context::ExactContextSnapshot,
        acquired_packages: &BTreeMap<
            ArtifactId,
            Arc<crate::certified_products::CertifiedPackageCodeProduct>,
        >,
        validation: &mut crate::recovery_artifacts::PackageInterfaceValidation,
    ) -> Result<Self, CompileError> {
        let components = snapshot
            .native_components
            .iter()
            .map(|(identity, record)| (*identity, Arc::clone(record.facts())))
            .collect();
        let closures = snapshot
            .native_closures
            .iter()
            .map(|(identity, record)| (*identity, Arc::clone(record.facts())))
            .collect();
        let issued = crate::linked_native::restore_acquired_native_closures(
            &components,
            &closures,
            &self.artifacts,
            acquired_packages,
            validation,
        )?;
        let mut restored = AcquiredNativeClosures::default();
        for closure in issued {
            crate::host_work::checkpoint()?;
            restored = restored.append(&AcquiredNativeClosures::one(closure)?)?;
        }
        // Restoration authenticated every grant against the held snapshot.
        // Attaching those owners must preserve its exact custody seeds, including
        // seeds that also occur as selected roots. Live custody composition may
        // normalize those seeds and is not a restoration operation.
        let mut draft = self.reopen();
        draft.native_closures = restored;
        draft.close()
    }

    pub(crate) fn selected_package_products(
        &self,
        selected: &BTreeSet<ArtifactId>,
    ) -> Result<
        BTreeMap<ArtifactId, Arc<crate::certified_products::CertifiedPackageCodeProduct>>,
        CompileError,
    > {
        Ok(selected
            .iter()
            .filter_map(|id| {
                self.native_closures
                    .package_product(id)
                    .map(|product| (*id, Arc::clone(product)))
            })
            .collect())
    }

    pub(crate) fn native_component_records(&self) -> &crate::exact_context::NativeComponentRecords {
        self.native_closures.component_records()
    }

    pub(crate) fn native_closure_records(
        &self,
    ) -> &crate::exact_context::NativeAcquiredClosureRecords {
        self.native_closures.closure_records()
    }

    pub(crate) fn acquired_native_closures(&self) -> &AcquiredNativeClosures {
        &self.native_closures
    }

    /// Import only already-issued immutable completion custody. Compiler roles,
    /// input decisions and availability stay with the receiving owner.
    pub(crate) fn with_linked_native_target(
        &self,
        target: &crate::linked_native::LinkedNativeTarget,
    ) -> Result<Self, CompileError> {
        // Repeated publication borrows the same immutable owner and does not
        // rebuild its scalar records or accumulate fresh wrapper owners.
        if target.acquired().len() == 0 || self.native_closures.contains_owner(target.closure()) {
            return Ok(self.clone());
        }
        let mut draft = self.reopen();
        draft.append_acquired_closures(&AcquiredNativeClosures::one(Arc::clone(
            target.closure(),
        ))?)?;
        draft.close()
    }

    pub(crate) fn with_acquired_native_closures_from(
        &self,
        incoming: &Self,
    ) -> Result<Self, CompileError> {
        let mut draft = self.reopen();
        draft.append_acquired_closures(&incoming.native_closures)?;
        draft.close()
    }

    pub(crate) fn native_offers(&self) -> &[IssuedNativeOffer] {
        &self.native_offers
    }

    pub(crate) fn close_request(
        &self,
        fragment: Option<&crate::declaration_context::OriginalCompilerInputs>,
        protected: &crate::declaration_context::ProtectedScaffoldRequirements,
    ) -> Result<Self, CompileError> {
        let support = match fragment {
            Some(fragment) => self.merge_output_fragments(fragment.compiler_source_support())?,
            None => self.clone(),
        };
        let projection = support.projection.merge(protected.role_requirements())?;
        // Protected requirements constrain existing admitted rows; they cannot
        // acquire another compiler owner from retained bytes.
        let support = support.rebuild_narrowed(
            projection,
            support.artifacts.clone(),
            support.native_offers.iter().cloned(),
        )?;
        support.resolve_input_policy()
    }

    pub(crate) fn from_recovered_selection(
        selection: crate::declaration_context::RecoveredCompilerSelection,
    ) -> Result<Self, CompileError> {
        let (artifacts, projection, issuance) = selection.into_parts();
        match issuance {
            crate::declaration_context::RecoveredCompilerIssuance::CheckedInput => {
                let support = Self::from_selected_issuance(projection, artifacts)?;
                let mut draft = support.reopen();
                draft.input_decisions = IssuedInputNativeDecisions::default()
                    .with_recorded_facts(draft.input_decisions.facts.clone());
                draft.close()
            }
            crate::declaration_context::RecoveredCompilerIssuance::RecordedRoles => {
                Self::new(projection, artifacts, [], Default::default())
            }
        }
    }

    /// Authenticate durable row records into actual acquisitions. Published
    /// birth records enter through this same boundary before they can become
    /// live publication authority; decoding alone never constructs live roles.
    pub(crate) fn authenticate_recorded_rows(
        artifacts: ArtifactView,
        rows: &imbl::OrdMap<ExactModuleIdentity, RecordedCompilerRow>,
        offers: &imbl::OrdMap<[u8; 32], crate::exact_context::NativeOfferSnapshotFacts>,
        availability: &imbl::OrdMap<
            ArtifactId,
            crate::declaration_context::NativeAvailabilitySnapshotFacts,
        >,
    ) -> Result<Self, CompileError> {
        let mut projection = CompilerInputProjection::default();
        let mut births = BTreeMap::new();
        for (owner, record) in rows {
            let entry = artifacts
                .lease
                .payloads
                .get(&record.interface())
                .ok_or_else(|| failure("recorded compiler row lacks its acquired interface"))?;
            if &entry.descriptor.owner != owner {
                return Err(failure(
                    "recorded compiler row differs from its acquired owner",
                ));
            }
            let role = match record {
                RecordedCompilerRow::InterfaceOnly { interface } => {
                    CompilerInputRole::InterfaceOnly {
                        interface: *interface,
                    }
                }
                RecordedCompilerRow::Original {
                    interface,
                    original,
                    provenance: RecordedOriginalProvenance::Reusable,
                    ..
                } => CompilerInputRole::ReusableOriginal {
                    interface: *interface,
                    original: *original,
                },
                RecordedCompilerRow::Original {
                    interface,
                    original,
                    provenance: RecordedOriginalProvenance::Published(birth),
                    ..
                } => {
                    if !matches!(birth.native_facts.roles.get(owner), Some(RecordedCompilerRow::Original {
                        interface: prior_interface, original: prior_original,
                        provenance: RecordedOriginalProvenance::Reusable, ..
                    }) if prior_interface == interface && prior_original == original)
                    {
                        return Err(failure(
                            "publication birth root is not its exact pre-endorsement Original",
                        ));
                    }
                    let ids = birth
                        .native_facts
                        .roles
                        .values()
                        .flat_map(|row| std::iter::once(row.interface()).chain(row.original()))
                        .collect();
                    let birth_artifacts = artifacts.select_issued(ids, &birth.selection)?;
                    let acquired = Self::authenticate_recorded_rows(
                        birth_artifacts,
                        &birth.native_facts.roles,
                        &birth.native_facts.offers,
                        &birth.native_facts.native_availability,
                    )?;
                    let acquired = Arc::new(acquired);
                    births.insert(owner.clone(), Arc::clone(&acquired));
                    CompilerInputRole::PublishedSourceOriginal {
                        interface: *interface,
                        original: *original,
                        source_revision: birth.source_revision.clone(),
                        original_input_identity: birth.original_input_identity.clone(),
                        selection: birth.selection.clone(),
                        selection_sha256: birth.selection_sha256,
                        native_authority: PublicationBirthFacts::from_recorded_facts(Arc::clone(
                            acquired.native_fact_owner(),
                        )),
                    }
                }
            };
            projection.admit_role(owner.clone(), role)?;
        }
        projection.validate(&artifacts)?;
        let mut selected = projection.resolve_associations(&artifacts)?;
        for (owner, birth) in births {
            let publication =
                PublishedOriginal::from_endorsement(&owner, &projection.roles[&owner], &birth)?
                    .expect("authenticated published role");
            if let SelectedCompilerRow::Original { provenance, .. } =
                &mut selected.get_mut(&owner).expect("authenticated Original").0
            {
                *provenance = OriginalProvenance::Published(publication);
            }
        }
        let acquired = Self::from_authenticated_rows(
            projection,
            artifacts,
            selected,
            [],
            Default::default(),
            Default::default(),
            None,
            Default::default(),
        )?;
        acquired.restore_native_maps(rows, offers, availability)
    }

    pub(crate) fn restore_snapshot_evidence(
        &self,
        snapshot: &crate::exact_context::ExactContextSnapshot,
        acquired_packages: &BTreeMap<
            ArtifactId,
            Arc<crate::certified_products::CertifiedPackageCodeProduct>,
        >,
        validation: &mut crate::recovery_artifacts::PackageInterfaceValidation,
    ) -> Result<Self, CompileError> {
        self.restore_native_code_records(snapshot, acquired_packages, validation)
    }

    fn restore_native_maps(
        &self,
        roles: &imbl::OrdMap<ExactModuleIdentity, RecordedCompilerRow>,
        offer_facts: &imbl::OrdMap<[u8; 32], crate::exact_context::NativeOfferSnapshotFacts>,
        availability_facts: &imbl::OrdMap<
            ArtifactId,
            crate::declaration_context::NativeAvailabilitySnapshotFacts,
        >,
    ) -> Result<Self, CompileError> {
        if self.projection.roles.len() != roles.len() {
            return Err(failure(
                "recovered availability has another compiler row domain",
            ));
        }
        let mut decision_facts = imbl::OrdMap::new();
        for (owner, record) in roles {
            let role =
                self.projection.roles.get(owner).ok_or_else(|| {
                    failure("recovered availability lost its acquired compiler row")
                })?;
            let mut identity = record.clone();
            if let RecordedCompilerRow::Original { native_choices, .. } = &mut identity {
                *native_choices = RecordedNativeChoices::Unissued;
            }
            if role.recorded_row() != identity {
                return Err(failure(
                    "recovered availability row differs from its acquired context",
                ));
            }
            if let Some(RecordedNativeChoices::Issued { groups }) = record.native_choices() {
                decision_facts.insert(
                    owner.clone(),
                    RecordedInputNativeDecision {
                        role: role.recorded_fact(),
                        groups: groups.clone(),
                    },
                );
            }
        }
        let offers = offer_facts
            .iter()
            .map(|(key, fact)| {
                if *key != fact.identity()? {
                    return Err(failure("native offer key differs from its issued facts"));
                }
                fact.restore(&self.artifacts)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let proof_map = offers
            .iter()
            .map(|offer| Ok((offer.snapshot_facts().identity()?, offer.clone())))
            .collect::<Result<BTreeMap<_, _>, CompileError>>()?;
        let availability = crate::declaration_context::RequestNativeAvailability::restore(
            availability_facts,
            &proof_map,
            &self.artifacts,
        )?;
        // Each restored offer checked its exact bindings, payloads and scopes
        // against this selection. Reattaching it does not issue new carrier
        // seeds or change the recorded compiler selection.
        let mut draft = self.reopen();
        draft.native_offers = offers.into();
        draft.native_offer_facts = offer_facts.clone();
        draft.native_availability = availability;
        let restored = draft.close()?;
        restored
            .native_availability
            .validate_role_coverage(restored.compiler_roles())?;
        let state = restored
            .artifacts
            .lease
            .inventory
            .0
            .lock()
            .expect("inventory lock");
        for (owner, fact) in &decision_facts {
            if restored
                .projection
                .snapshot_roles()
                .get(owner)
                .is_none_or(|role| &fact.role != role)
            {
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
        let mut draft = restored.reopen();
        let mut selected = restored.selected.as_ref().clone();
        for (owner, fact) in decision_facts {
            let row = selected
                .rows
                .get(&owner)
                .expect("authenticated recorded role")
                .clone()
                .with_native_choice(fact.groups, |_| SelectedNativeSupport::missing());
            selected.changed_owners.insert(owner.clone());
            selected.rows.insert(owner, row);
        }
        draft.selected = Arc::new(selected);
        draft.close()
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
                vec![Arc::clone(&entry)],
                &namespace.interface_only(),
                &[],
            )?;
        let joined = joined.merge_selected(&source.artifacts, &namespace)?;
        source.append_produced_interfaces(namespace, joined, [entry])
    }

    pub(crate) fn close_complete_output(
        output: &crate::declaration_context::OriginalCompilerInputs,
    ) -> Result<Self, CompileError> {
        let support = output.compiler_source_support();
        support.projection.validate_selected_requirements(
            &support
                .selected
                .rows
                .iter()
                .map(|(owner, row)| (owner.clone(), row.clone()))
                .collect(),
        )?;
        Ok(support.clone())
    }

    pub(crate) fn from_original_output(
        compiler: &Self,
        parent: &Self,
        output: &crate::declaration_context::OriginalCompilerInputs,
    ) -> Result<Self, CompileError> {
        let retained = compiler.rebuild_narrowed(
            compiler.projection.within_view(output.issued_artifacts()),
            output.issued_artifacts().clone(),
            parent.native_offers().iter().cloned(),
        )?;
        let support = retained.merge_output_fragments(output.compiler_source_support())?;
        support.with_acquired_native_closures_from(parent)
    }

    /// Issue only the role fragment named by this actual checked stage/source
    /// selection. The receiving prefix validates the same exact rows on append.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn world_stage_fragment(
        program: &crate::compiler_world::CheckedWorldCellProgram,
        stage: &Arc<crate::compiler_world::CheckedWorldCellStage>,
        source: &crate::declaration_context::ExactSourceAdmission,
        source_text: &str,
        output: &crate::declaration_context::IssuedOriginalOutput,
        produced: &crate::checked_cell::ProducedValueTypeInterfaces,
    ) -> Result<Self, CompileError> {
        if !source.matches_world_stage(program, stage.index(), source_text)? {
            return Err(failure(
                "program source fragment belongs to another World stage",
            ));
        }
        let generated = source.generated_source_owner();
        let mut owners = source.compiler_dependencies().owners();
        let executable = matches!(
            stage.identity(),
            crate::compiler_world::WorldCellStageIdentity::Executable { .. }
        );
        let inputs = output.compiler_inputs();
        let artifacts = inputs.issued_artifacts();
        let mut available = inputs.role_projection().clone();
        if executable {
            owners.remove(&generated);
            available =
                available.project_roles(|owner, role| (owner != &generated).then(|| role.clone()));
        }
        let available = available.world_stage_roles(&owners);
        let operation = program.validation().inventory.as_ref();
        let charge = |result: Result<(), tidepool_repr::execution_schema::ParseError>| {
            result.map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))
        };
        let push = |pending: &mut Vec<ExactModuleIdentity>, owner: &ExactModuleIdentity| {
            charge(operation.reserve::<ExactModuleIdentity>(1))?;
            charge(operation.charge(owner.unit.len().saturating_add(owner.module.len())))?;
            pending.push(owner.clone());
            Ok::<_, CompileError>(())
        };
        let mut pending = Vec::new();
        for owner in &owners {
            push(&mut pending, owner)?;
        }
        for output in produced.required() {
            crate::host_work::checkpoint()?;
            let interface = output.interface();
            charge(operation.charge(interface.unit().len() + interface.module().len()))?;
            let owner = ExactModuleIdentity {
                unit: interface.unit().to_owned(),
                module: interface.module().to_owned(),
            };
            if available.roles.get(&owner).is_none_or(|role| {
                role.interface() != output.artifact_id() || role.original().is_some()
            }) {
                return Err(failure(
                    "World source fragment lost its actual produced TYPE output",
                ));
            }
            push(&mut pending, &owner)?;
        }
        // A source fragment carries its own roots and required TYPE closure.
        // Unrelated checked values remain stage inputs, not new prefix roles.
        let mut projection = CompilerInputProjection::default();
        while let Some(owner) = pending.pop() {
            crate::host_work::checkpoint()?;
            charge(operation.charge(1))?;
            if projection.roles.contains_key(&owner) {
                continue;
            }
            let role = available
                .roles
                .get(&owner)
                .ok_or_else(|| failure("World source fragment lacks a required compiler role"))?;
            let role = role.clone();
            for id in std::iter::once(role.interface()).chain(role.original()) {
                let entry =
                    artifacts.lease.payloads.get(&id).ok_or_else(|| {
                        failure("World source fragment lacks required TYPE custody")
                    })?;
                for required in &entry.requirements {
                    push(&mut pending, required)?;
                }
            }
            projection.admit_role(owner, role)?;
        }
        let selected = projection
            .roles
            .iter()
            .map(|(owner, role)| {
                let mut row = inputs
                    .compiler_roles()
                    .selected_row(owner)
                    .ok_or_else(|| failure("World source fragment lost its actual selected row"))?
                    .clone();
                if role.original().is_none() {
                    row.select_type_only();
                }
                Ok((owner.clone(), row.with_issued_provenance(role)?))
            })
            .collect::<Result<BTreeMap<_, _>, CompileError>>()?;
        projection.validate_requirements(
            selected
                .values()
                .flat_map(|row| row.bindings().map(|(_, entry)| entry.as_ref())),
            |owner| selected.get(owner).map(|row| row.interface().1.as_ref()),
        )?;
        let input_decisions = inputs
            .issued_input_decisions()
            .cloned()
            .unwrap_or_default()
            .for_exact_roles(&projection);
        let selected = Arc::new(SelectedCompilerRows::from_admitted(selected));
        let native_availability = inputs
            .native_availability()
            .for_compiler_roles(selected.admitted_roles(), artifacts)?;
        if executable
            && native_availability.rows().any(|row| {
                row.original_owner().unit == generated.unit
                    && row.original_owner().module == generated.module
            })
        {
            return Err(failure("program support depends on its generated scaffold"));
        }
        let mut offers = Vec::new();
        let mut native_offer_facts = imbl::OrdMap::new();
        for offer in native_availability.rows().flat_map(|row| row.offers()) {
            let identity = offer.snapshot_identity()?;
            if !native_offer_facts.contains_key(&identity) {
                native_offer_facts.insert(identity, offer.snapshot_facts());
                offers.push(offer.clone());
            }
        }
        // Completion custody joins the acquired artifacts before the draft
        // exists; `close` never re-folds it.
        let artifacts = inputs
            .acquired_native_closures()
            .iter()
            .try_fold(artifacts.clone(), |view, completion| {
                view.retain_custody(completion.artifacts())
            })?;
        SupportDraft {
            projection,
            artifacts,
            native_fact_owner: inputs.native_fact_base().cloned(),
            native_offers: offers.into(),
            native_offer_facts,
            native_closures: inputs.acquired_native_closures().clone(),
            native_availability,
            selected,
            input_decisions,
        }
        .close()
    }

    pub(crate) fn extend_program_source(
        &self,
        selection: &crate::certified_products::CertifiedSourceSelection,
        artifacts: &ArtifactView,
        issued: &CompilerInputProjection,
        dependencies: &crate::declaration_context::CompilerDependencies,
    ) -> Result<Self, CompileError> {
        let incoming = issued.for_source_owners(&dependencies.owners());
        let selected = Self::certified_output_rows(selection, &incoming, artifacts)?;
        let availability = crate::declaration_context::RequestNativeAvailability::from_issued_rows(
            selection.issued_native_availability().cloned(),
        )?;
        let fragment = Self::from_authenticated_rows(
            incoming,
            artifacts.clone(),
            selected,
            selection
                .issued_native_availability()
                .flat_map(|row| row.offers().cloned()),
            availability,
            IssuedInputNativeDecisions::default(),
            selection.native_fact_base(),
            selection.acquired_native_closures(),
        )?;
        self.merge_output_fragments(&fragment)
    }

    pub(crate) fn empty() -> Self {
        Self::new(
            CompilerInputProjection::default(),
            ArtifactInventory::default().empty_view(),
            [],
            Default::default(),
        )
        .expect("empty compiler support is closed")
    }

    pub(crate) fn retain_custody(&self, incoming: &ArtifactView) -> Result<Self, CompileError> {
        let artifacts = self.artifacts.retain_custody(incoming)?;
        let artifacts =
            artifacts.retain_issued_offer_custody(self.native_offers.iter().cloned())?;
        // Custody attachment preserves the exact namespace and executable
        // roots and the actual selected acquisition owners.
        SupportDraft {
            projection: self.projection.clone(),
            artifacts,
            native_offers: Arc::clone(&self.native_offers),
            native_fact_owner: Some(Arc::clone(&self.native_fact_owner)),
            native_offer_facts: self.native_offer_facts.clone(),
            native_closures: self.native_closures.clone(),
            native_availability: self.native_availability.clone(),
            selected: Arc::clone(&self.selected),
            input_decisions: self.input_decisions.clone(),
        }
        .close()
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
        let mut native_offer_facts = self.native_offer_facts.clone();
        for offer in &incoming {
            let identity = offer.snapshot_identity()?;
            if !native_offer_facts.contains_key(&identity) {
                native_offer_facts.insert(identity, offer.snapshot_facts());
            }
        }
        let mut offers = Vec::new();
        let artifacts = self.artifacts.retain_issued_offer_custody(
            self.native_offers
                .iter()
                .cloned()
                .chain(incoming)
                .filter_map(|offer| {
                    if offers
                        .iter()
                        .any(|prior: &IssuedNativeOffer| prior.same_issuance(&offer))
                    {
                        None
                    } else {
                        offers.push(offer.clone());
                        Some(offer)
                    }
                }),
        )?;
        // Raw offers add checked custody only. They cannot change the admitted
        // namespace, normalized permissions or recorded execution decisions.
        SupportDraft {
            projection: self.projection.clone(),
            artifacts,
            native_fact_owner: Some(Arc::clone(&self.native_fact_owner)),
            native_offer_facts,
            native_closures: self.native_closures.clone(),
            native_offers: offers.into(),
            native_availability: self.native_availability.clone(),
            selected: Arc::clone(&self.selected),
            input_decisions: self.input_decisions.clone(),
        }
        .close()
    }

    pub(crate) fn bind_snapshot_baseline(
        &self,
        snapshot: &crate::exact_context::ExactContextSnapshot,
    ) -> Result<Self, CompileError> {
        let mut projection = self.projection.clone();
        projection.bind_snapshot_roles(&snapshot.roles)?;
        let mut artifacts = self.artifacts.clone();
        artifacts.bind_selection_snapshot_baseline(snapshot.selection().clone())?;
        let mut draft = self.reopen();
        draft.projection = projection;
        draft.artifacts = artifacts;
        draft.native_closures = self.native_closures.bind_record_baseline(snapshot)?;
        draft.close()
    }

    pub(crate) fn finalize_namespace(&self) -> Result<Self, CompileError> {
        match self.artifacts.with_compiler_projection(&self.projection)? {
            std::borrow::Cow::Borrowed(_) => Ok(self.clone()),
            std::borrow::Cow::Owned(artifacts) => self.rebuild(
                self.projection.clone(),
                artifacts,
                self.native_offers.iter().cloned(),
            ),
        }
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
            for (_, entry) in row.bindings() {
                entries.insert(entry.descriptor.id, Arc::clone(entry));
            }
        }
        // This operation changes policy over existing selected bindings only.
        // Canonical requirements may differ from an original's requirements,
        // so validate the resulting actual dependency/seal relationships.
        projection.entries_from_artifacts(&entries)?;
        let mut selected = self.selected.rows.clone();
        for (owner, role) in &projection.roles {
            let row = selected.get_mut(owner).expect("existing compiler role");
            if row.interface().0.artifact != role.interface() {
                return Err(failure(
                    "source policy changed its admitted canonical binding",
                ));
            }
            match (role.original(), row.original()) {
                (None, _) => row.select_type_only(),
                (Some(id), Some((binding, _))) if id == binding.artifact => {}
                _ => {
                    return Err(failure(
                        "source policy changed its admitted original binding",
                    ));
                }
            }
        }
        let input_decisions = self.input_decisions.for_exact_roles(&projection);
        let selected = Arc::new(SelectedCompilerRows {
            rows: selected,
            home_units: Arc::clone(&self.selected.home_units),
            changed_owners: projection.roles.keys().cloned().collect(),
        });
        SupportDraft {
            native_availability: self
                .native_availability
                .for_compiler_roles(selected.admitted_roles(), &self.artifacts)?,
            projection,
            artifacts: self.artifacts.clone(),
            native_offers: Arc::clone(&self.native_offers),
            native_fact_owner: Some(Arc::clone(&self.native_fact_owner)),
            native_offer_facts: self.native_offer_facts.clone(),
            native_closures: self.native_closures.clone(),
            selected,
            input_decisions,
        }
        .close()
    }

    pub(crate) fn certify_original_closure(
        &self,
        originals: &crate::certified_products::SelectedOriginalClosure,
    ) -> Result<Self, CompileError> {
        originals.compiler_source_support().merge(self)
    }

    pub(crate) fn with_certified_original_choices(
        &self,
        selection: &crate::certified_products::CertifiedSourceSelection,
        native: &ArtifactView,
    ) -> Result<Self, CompileError> {
        let mut selected = self.selected.as_ref().clone();
        for (owner, row) in self
            .selected
            .iter()
            .filter(|(_, row)| row.original().is_some())
        {
            let original = row.original().expect("selected Original");
            let groups = checked_input_group_choices(
                original.0.artifact,
                native.roots().iter().filter_map(|node| match node {
                    InventoryNodeKey::Group(group)
                        if group.binding.artifact() == original.0.artifact =>
                    {
                        Some(*group)
                    }
                    _ => None,
                }),
            )?;
            let mut supports = imbl::OrdMap::new();
            for group in groups.values().copied() {
                let key = NativeGroupKey {
                    artifact: original.0.artifact,
                    original_ordinal: group.original_ordinal,
                };
                let Some(crate::certified_products::CertifiedNativeGroupSupport::Issued(offer)) =
                    selection.native_group_support(
                        key,
                        match &original.1.payload {
                            ArtifactPayload::Original(product) => product.owner(),
                            _ => unreachable!("Original row"),
                        },
                        group,
                    )
                else {
                    return Err(native_admission_failure(
                        key,
                        NativeAdmissionRefusal::InputIssuedSupportMissing { candidate: group },
                    ));
                };
                if !offer
                    .original_acquisition()
                    .same_issuing_authority(&original.1)
                {
                    return Err(failure(
                        "selected original closure crossed acquired output owners",
                    ));
                }
                supports.insert(
                    group,
                    SelectedNativeSupport::Acquired(Arc::from([offer.clone()])),
                );
            }
            let row = row
                .clone()
                .with_native_choice(groups, |group| supports[&group].clone());
            selected.changed_owners.insert(owner.clone());
            selected.rows.insert(owner.clone(), row);
        }
        let mut draft = self.reopen();
        draft.artifacts = self.artifacts.retain_custody(native)?;
        draft.selected = Arc::new(selected);
        draft.close()
    }

    #[tracing::instrument(
        target = "exomonad_harness::timing", name = "exact.value_interface_admission",
        level = "debug", skip_all, fields(inclusive = true, value_count = values.len())
    )]
    pub(crate) fn admit_value_interfaces(
        &self,
        values: &[Arc<crate::recovery_artifacts::CertifiedValueInterface>],
    ) -> Result<Self, CompileError> {
        let mut timing = crate::timing::InvocationTiming::start_scoped(
            "exact.value_interface_admission",
            "create_entries",
        );
        if values.is_empty() {
            timing.finish(true);
            return Ok(self.clone());
        }
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
        timing.advance("merge_projection");
        let (projection, entries) = match self
            .artifacts
            .inventory()
            .prepare_value_interface_append(self, entries)?
        {
            ValueInterfaceAdmission::Append(append) => {
                let appended = append.issue()?;
                let mut rows = self.selected.rows.clone();
                for (owner, interface) in appended.selected {
                    rows.insert(
                        owner,
                        ResolvedCompilerRole::from_issued_bindings(interface, None),
                    );
                }
                // The inventory appended only interface rows with checked
                // targets. Earlier exact namespace bindings, scopes, roots and
                // acquisitions remain owned by the same parent snapshot.
                let changed_owners = appended.projection.roles.keys().cloned().collect();
                let draft = SupportDraft {
                    projection: appended.projection,
                    artifacts: appended.artifacts,
                    selected: Arc::new(SelectedCompilerRows {
                        rows,
                        home_units: Arc::clone(&self.selected.home_units),
                        changed_owners,
                    }),
                    native_offers: Arc::clone(&self.native_offers),
                    native_fact_owner: Some(Arc::clone(&self.native_fact_owner)),
                    native_offer_facts: self.native_offer_facts.clone(),
                    native_closures: self.native_closures.clone(),
                    native_availability: self.native_availability.clone(),
                    input_decisions: self.input_decisions.clone(),
                };
                timing.finish(true);
                return draft.close();
            }
            ValueInterfaceAdmission::General {
                projection,
                entries,
                reason,
            } => {
                match reason {
                    ValueInterfaceGeneralAdmission::RetainedOwner(owner) => {
                        tracing::trace!(?owner, "value interface uses general owner admission");
                    }
                    ValueInterfaceGeneralAdmission::UnselectedRepeat(artifact)
                    | ValueInterfaceGeneralAdmission::MissingNamespaceBinding(artifact)
                    | ValueInterfaceGeneralAdmission::DifferentRequirementBindings(artifact) => {
                        tracing::trace!(
                            ?artifact,
                            "value interface requires general binding admission"
                        );
                    }
                }
                (projection, entries)
            }
        };
        timing.advance("native_dependency_catalog");
        let catalog = NativeDependencyCatalog::from_source(self)?;
        timing.advance("admit_interfaces");
        let artifacts = projection.admit_interfaces_with_native_offers(
            &self.artifacts,
            entries.clone(),
            &self.native_offers,
            &catalog,
        )?;
        timing.advance("rebuild_support");
        let result = self.append_produced_interfaces(projection, artifacts, entries);
        timing.finish(result.is_ok());
        result
    }

    pub(crate) fn without_value_roots(
        &self,
        excluded: &BTreeSet<ArtifactId>,
    ) -> Result<Self, CompileError> {
        let artifacts = self.artifacts.without_value_interface_roots(excluded)?;
        let projection = self.projection.within_view(&artifacts);
        self.rebuild(projection, artifacts, self.native_offers.iter().cloned())
    }

    /// Initialize one stage's compiler prefix. Future value rows can remain
    /// private exact dependencies, but cannot remain public compiler roles.
    pub(super) fn segment_value_prefix(
        &self,
        excluded: &BTreeSet<ArtifactId>,
    ) -> Result<Self, CompileError> {
        let artifacts = self.artifacts.without_value_interface_roots(excluded)?;
        let projection = self
            .projection
            .project_roles(|_, role| (!excluded.contains(&role.interface())).then(|| role.clone()));
        let artifacts = artifacts
            .with_compiler_projection(&projection)?
            .into_owned();
        self.rebuild(projection, artifacts, self.native_offers.iter().cloned())
    }

    /// The freshly checked value certificate selects its exact canonical role;
    /// retained artifact IDs are not an input to this boundary.
    pub(crate) fn select_value_interface(
        &self,
        certificate: &crate::recovery_artifacts::CertifiedValueInterface,
    ) -> Result<Self, CompileError> {
        let selected = self.selected_interfaces()?;
        let owner = ExactModuleIdentity {
            unit: certificate.interface().unit().to_owned(),
            module: certificate.interface().module().to_owned(),
        };
        if selected
            .get(&owner)
            .is_none_or(|entry| entry.descriptor.id != certificate.artifact_id())
        {
            return Err(failure(
                "value certificate is outside selected compiler interfaces",
            ));
        }
        selected.support_for_owners(vec![owner])
    }

    /// Reopen only an already acquired publication's exact pre-endorsement
    /// owner. This consumes its retained admission rows, never equal inventory.
    pub(crate) fn publication_birth_support(
        &self,
        root: &ExactModuleIdentity,
    ) -> Result<Self, CompileError> {
        let birth = self
            .selected
            .get(root)
            .and_then(ResolvedCompilerRole::publication)
            .ok_or_else(|| failure("publication birth lacks its acquired published Original"))?;
        if birth.birth.artifacts.selection_snapshot().as_ref()
            != birth.facts.selection.snapshot().as_ref()
        {
            return Err(failure(
                "publication native owner differs from its issued selection",
            ));
        }
        Ok(birth.birth.as_ref().clone())
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
        if role.is_published_source_original() {
            let prior = self.publication_birth_support(root)?;
            let endorsement =
                crate::declaration_context::PublishedOriginalEndorsement::from_acquired_support(
                    self, root,
                )?;
            // Validate the immutable issuer independently. Its roles and demand
            // are historical evidence, not the receiving compiler projection.
            prior.validate_publication_endorsement(&endorsement)?;
            if roots
                .iter()
                .any(|id| !self.artifacts.contains_namespace_artifact(*id))
            {
                return Err(failure(
                    "publication root is outside the current compiler namespace",
                ));
            }
            return self
                .source_support(owners)?
                .retain_custody(prior.artifacts());
        }
        let artifacts = self.artifacts.select_publication_roots(original, roots)?;
        let projection = self.projection.project_roles(|_, role| {
            artifacts
                .contains_namespace_artifact(role.interface())
                .then(|| {
                    if let Some(original) = role
                        .original()
                        .filter(|id| artifacts.contains_namespace_artifact(*id))
                    {
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

    fn validate_publication_endorsement(
        &self,
        endorsement: &crate::declaration_context::PublishedOriginalEndorsement,
    ) -> Result<&CompilerInputRole, CompileError> {
        let role = endorsement.role();
        let previous = self
            .projection
            .snapshot_roles()
            .get(endorsement.owner())
            .ok_or_else(|| failure("publication lacks its issued compiler role"))?;
        let CompilerInputRole::PublishedSourceOriginal {
            selection,
            native_authority,
            ..
        } = role
        else {
            return Err(failure("publication endorsement lacks published policy"));
        };
        let birth = endorsement.birth();
        if birth.native_fact_owner().as_ref() != native_authority.facts().as_ref() {
            return Err(failure(
                "publication birth facts differ from actual support",
            ));
        }
        // The exact fact domain and the acquired admission owners are separate
        // obligations. Equal IDs/graphs cannot substitute another birth's mode.
        if self.native_fact_owner().commitment() != birth.native_fact_owner().commitment()
            || !Arc::ptr_eq(&self.selected, &birth.selected)
            || !Arc::ptr_eq(&self.artifacts.lease, &birth.artifacts.lease)
            || !Arc::ptr_eq(&self.native_offers, &birth.native_offers)
            || !self
                .native_availability
                .same_acquisition(&birth.native_availability)
        {
            return Err(admission_failure(
                ArtifactInventoryFailure::PublicationNativeDomainMismatch {
                    owner: endorsement.owner().clone(),
                },
            ));
        }
        if previous.interface() != role.interface()
            || previous.original() != role.original()
            || selection.graph_selection() != &self.artifacts.capture_graph_selection()
        {
            return Err(failure(
                "publication endorsement differs from its exact issuer",
            ));
        }
        Ok(previous)
    }

    pub(crate) fn publish_original(
        &self,
        endorsement: &crate::declaration_context::PublishedOriginalEndorsement,
    ) -> Result<Self, CompileError> {
        self.validate_publication_endorsement(endorsement)?;
        let role = endorsement.role();
        let mut selected = self.selected.as_ref().clone();
        selected.changed_owners.insert(endorsement.owner().clone());
        let row = selected
            .rows
            .get_mut(endorsement.owner())
            .expect("checked publication owner");
        let birth =
            PublishedOriginal::from_endorsement(endorsement.owner(), role, endorsement.birth())?
                .ok_or_else(|| failure("publication endorsement lacks its birth"))?;
        if let SelectedCompilerRow::Original { provenance, .. } = &mut row.0 {
            *provenance = OriginalProvenance::Published(birth);
        }

        let mut draft = self.reopen();
        draft.selected = Arc::new(selected);
        draft.close()
    }

    pub(crate) fn select_support_roots(
        &self,
        roots: Vec<ArtifactId>,
    ) -> Result<Self, CompileError> {
        let artifacts = self.artifacts.select_roots(roots)?;
        let projection = self.projection.for_selected_support(&artifacts);
        self.rebuild_narrowed(projection, artifacts, self.native_offers.iter().cloned())
    }

    pub(crate) fn source_support(
        &self,
        owners: &BTreeSet<ExactModuleIdentity>,
    ) -> Result<Self, CompileError> {
        let mut projection = CompilerInputProjection::default();
        let mut selected = BTreeMap::new();
        let mut pending = owners
            .iter()
            .filter(|owner| self.compiler_roles().get(owner).is_some())
            .cloned()
            .collect::<Vec<_>>();
        while let Some(owner) = pending.pop() {
            if projection.roles.contains_key(&owner) {
                continue;
            }
            let previous = self
                .projection
                .roles
                .get(&owner)
                .ok_or_else(|| failure("source compiler dependency lacks its issued role"))?;
            let role = if owners.contains(&owner) {
                previous.clone()
            } else {
                CompilerInputRole::InterfaceOnly {
                    interface: previous.interface(),
                }
            };
            let mut row = self.selected[&owner].clone();
            if role.original().is_none() {
                row.select_type_only();
            }
            let entry = row.original().unwrap_or(row.interface());
            pending.extend(entry.1.requirements.iter().cloned());
            selected.insert(owner.clone(), row);
            projection.admit_role(owner, role)?;
        }
        let entries = selected
            .values()
            .flat_map(|row| row.bindings())
            .map(|(binding, entry)| (binding.artifact, Arc::clone(entry)))
            .collect();
        projection.entries_from_artifacts(&entries)?;
        // Keep the existing topology/custody transition. Only its already
        // authenticated role rows bypass metadata reconstruction afterward.
        let artifacts = self.artifacts.select_compiler_support(&projection)?;
        let selected = Arc::new(SelectedCompilerRows::from_admitted(selected));
        let decisions = self.input_decisions.restricted(&projection, &artifacts);
        let mut native_offer_facts = self.native_offer_facts.clone();
        let offers = self
            .native_offers
            .iter()
            .filter_map(
                |offer| match offer.is_relevant_to_support(selected.admitted_roles()) {
                    Ok(true) => Some(Ok(offer.clone())),
                    Ok(false) => match offer.snapshot_identity() {
                        Ok(identity) => {
                            native_offer_facts.remove(&identity);
                            None
                        }
                        Err(error) => Some(Err(error)),
                    },
                    Err(error) => Some(Err(error)),
                },
            )
            .collect::<Result<Vec<_>, _>>()?;
        let artifacts = artifacts.retain_issued_offer_custody(offers.iter().cloned())?;
        SupportDraft {
            native_availability: self
                .native_availability
                .for_compiler_roles(selected.admitted_roles(), &self.artifacts)?,
            projection,
            artifacts,
            native_fact_owner: Some(Arc::clone(&self.native_fact_owner)),
            native_offer_facts,
            native_closures: self.native_closures.clone(),
            native_offers: offers.into(),
            selected,
            input_decisions: decisions,
        }
        .retain_completion_custody()?
        .close()
    }

    /// Type evidence adds selected interfaces while the receiver continues to
    /// own its exact execution demand independently of published source policy.
    pub(crate) fn extend_interfaces(
        &self,
        interfaces: &SelectedCompilerInterfaces<'_>,
    ) -> Result<Self, CompileError> {
        let incoming = interfaces
            .support_for_owners(interfaces.iter().map(|(owner, _)| owner.clone()).collect())?;
        self.merge(&incoming)
    }

    #[tracing::instrument(
        target = "exomonad_harness::timing", name = "exact.compiler_support.merge",
        level = "debug", skip_all,
        fields(inclusive = true, left_roles = self.projection.roles.len(), right_roles = other.projection.roles.len(),
            left_offers = self.native_offers.len(), right_offers = other.native_offers.len())
    )]
    pub(crate) fn merge(&self, other: &Self) -> Result<Self, CompileError> {
        let mut timing = crate::timing::InvocationTiming::start_scoped(
            "exact.compiler_support.merge",
            "merge_compiler_inputs_with_demand",
        );
        let inputs = ArtifactView::merge_admitted_compiler_supports(self, other)?;
        timing.advance("validate_carried_associations");
        // The solver carries each whole selected row from its actual issuer.
        // Additional custody cannot replace those acquired inputs.
        inputs
            .projection
            .validate_selected_requirements(&inputs.selected)?;
        let support = Self::complete_resolved(
            inputs,
            self.native_offers
                .iter()
                .chain(other.native_offers.iter())
                .cloned(),
            self.native_availability.clone(),
            Some(&self.native_fact_owner),
            self.native_closures.append(&other.native_closures)?,
            &mut timing,
        )?
        .close()?;
        timing.advance("retain_normalized_availability");
        let result = support.retain_normalized_availability(other.native_availability());
        drop(support);
        timing.finish(result.is_ok());
        result
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

enum CompilerRolePayloads<'a> {
    Retained(RetainedArtifactPayloads<'a>),
    Metadata(&'a BTreeMap<ArtifactId, Arc<ArtifactEntry>>),
    PersistentMetadata(&'a imbl::OrdMap<ArtifactId, Arc<ArtifactEntry>>),
}

impl CompilerRolePayloads<'_> {
    fn get(&self, id: &ArtifactId) -> Option<&Arc<ArtifactEntry>> {
        match self {
            Self::Retained(payloads) => payloads.get(id),
            Self::Metadata(payloads) => payloads.get(id),
            Self::PersistentMetadata(payloads) => payloads.get(id),
        }
    }
}

impl CompilerInputProjection {
    pub fn role_view(&self) -> AssemblyRoleView<'_> {
        AssemblyRoleView(self)
    }

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
            let Some(consumed) = offer.within_assembly(self.role_view(), parent)? else {
                continue;
            };
            if originals.contains(&consumed.original()) || consumed.offers_groups_for(&originals) {
                contributions.push(consumed);
            } else if !consumed.qualifies_assembly(self.role_view(), parent) {
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
        CompilerSourceSupport::new(projection, artifacts, [], Default::default())
    }

    pub(crate) fn snapshot_roles(&self) -> &imbl::OrdMap<ExactModuleIdentity, CompilerInputRole> {
        &self.roles
    }
    pub(crate) fn recorded_roles(&self) -> &imbl::OrdMap<ExactModuleIdentity, RecordedCompilerRow> {
        &self.recorded_roles
    }
    pub(crate) fn bind_snapshot_roles(
        &mut self,
        roles: &imbl::OrdMap<ExactModuleIdentity, RecordedCompilerRow>,
    ) -> Result<(), CompileError> {
        if &self.recorded_roles != roles {
            return Err(failure(
                "restored compiler roles differ from exact snapshot",
            ));
        }
        // Equality checks logical facts; retain per-context acquired role owners.
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
        let mut projected = self.clone();
        for (owner, previous) in &self.roles {
            match select(owner, previous) {
                Some(role) if &role == previous => {}
                Some(role) => {
                    projected
                        .recorded_roles
                        .insert(owner.clone(), role.recorded_row());
                    projected.roles.insert(owner.clone(), role);
                }
                None => {
                    projected.roles.remove(owner);
                    projected.recorded_roles.remove(owner);
                }
            }
        }
        projected
    }

    /// A World stage selects compiler implementations only for its direct
    /// source imports. Dependencies remain part of that issuer's native
    /// availability without being promoted into compiler namespace roles.
    pub(crate) fn world_stage_roles(&self, source_owners: &BTreeSet<ExactModuleIdentity>) -> Self {
        self.project_roles(|owner, role| {
            Some(if source_owners.contains(owner) {
                role.clone()
            } else {
                CompilerInputRole::InterfaceOnly {
                    interface: role.interface(),
                }
            })
        })
    }

    pub(crate) fn from_issued_entries(
        entries: &[Arc<ArtifactEntry>],
    ) -> Result<Self, CompileError> {
        let mut projection = Self::default();
        for entry in entries {
            let (interface, original_reuse) = match &entry.payload {
                ArtifactPayload::Original(product) => (
                    ArtifactEntry::canonical_artifact_id(product.module_interface()),
                    Some(entry.descriptor.id),
                ),
                ArtifactPayload::Canonical(_) | ArtifactPayload::Interface(_, _) => {
                    (entry.descriptor.id, None)
                }
                ArtifactPayload::PackageCode(_) => continue,
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

    fn choose_role(
        owner: &ExactModuleIdentity,
        previous: &CompilerInputRole,
        incoming: &CompilerInputRole,
    ) -> Result<CompilerRoleIssuer, CompileError> {
        if previous == incoming {
            return Ok(CompilerRoleIssuer::Receiving);
        }
        if let (Some(existing), Some(new)) = (previous.original(), incoming.original()) {
            if existing != new {
                return Err(admission_failure(
                    ArtifactInventoryFailure::CompilerOriginalOfferConflict {
                        owner: owner.clone(),
                        existing,
                        incoming: new,
                    },
                ));
            }
        }
        if previous.interface() != incoming.interface() {
            return Err(ArtifactInventoryError {
                failure: ArtifactInventoryFailure::OwnerConflict {
                    owner: owner.clone(),
                },
                diagnostic_artifacts: None,
                native_refusal: None,
                owner_conflict: Some(ArtifactOwnerConflictEvidence::CompilerRole {
                    existing: previous.clone(),
                    incoming: incoming.clone(),
                }),
            }
            .into());
        }
        if previous.is_published_source_original() && incoming.is_published_source_original() {
            return Err(failure("conflicting published source original selection"));
        }
        Ok(match (previous.original(), incoming.original()) {
            (None, Some(_)) => CompilerRoleIssuer::Incoming,
            _ if incoming.is_published_source_original() => CompilerRoleIssuer::Incoming,
            _ => CompilerRoleIssuer::Receiving,
        })
    }

    pub(crate) fn admit_role(
        &mut self,
        owner: ExactModuleIdentity,
        incoming: CompilerInputRole,
    ) -> Result<(), CompileError> {
        if let Some(previous) = self.roles.get(&owner) {
            if matches!(
                Self::choose_role(&owner, previous, &incoming)?,
                CompilerRoleIssuer::Receiving
            ) {
                return Ok(());
            }
        }
        self.recorded_roles
            .insert(owner.clone(), incoming.recorded_row());
        self.roles.insert(owner, incoming);
        Ok(())
    }

    pub(super) fn retain_role_selection(&mut self, selected: &SelectedCompilerRole<'_>) {
        if matches!(selected.issuer, CompilerRoleIssuer::Incoming) {
            self.recorded_roles
                .insert(selected.owner.clone(), selected.role.recorded_row());
            self.roles
                .insert(selected.owner.clone(), selected.role.clone());
        }
    }

    /// Select each role and its issuer in one decision. Equal roles preserve
    /// receiving precedence without asking equality to recover ownership later.
    pub(super) fn merged_roles<'a>(
        &'a self,
        incoming: &'a Self,
    ) -> impl Iterator<Item = Result<SelectedCompilerRole<'a>, CompileError>> + 'a {
        let mut receiving = self.roles.iter().peekable();
        let mut incoming = incoming.roles.iter().peekable();
        std::iter::from_fn(move || {
            use std::cmp::Ordering;
            let ordering = match (receiving.peek(), incoming.peek()) {
                (Some((left, _)), Some((right, _))) => left.cmp(right),
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (None, None) => return None,
            };
            Some(match ordering {
                Ordering::Less => {
                    let (owner, role) = receiving.next().expect("receiving role");
                    Ok(SelectedCompilerRole {
                        owner,
                        role,
                        issuer: CompilerRoleIssuer::Receiving,
                    })
                }
                Ordering::Greater => {
                    let (owner, role) = incoming.next().expect("incoming role");
                    Ok(SelectedCompilerRole {
                        owner,
                        role,
                        issuer: CompilerRoleIssuer::Incoming,
                    })
                }
                Ordering::Equal => {
                    let (owner, previous) = receiving.next().expect("receiving role");
                    let (_, next) = incoming.next().expect("incoming role");
                    Self::choose_role(owner, previous, next).map(|issuer| SelectedCompilerRole {
                        owner,
                        role: match issuer {
                            CompilerRoleIssuer::Receiving => previous,
                            CompilerRoleIssuer::Incoming => next,
                        },
                        issuer,
                    })
                }
            })
        })
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
            entries: self.entries_from_metadata(&metadata)?.into_iter().collect(),
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
                let interface = product.module_interface();
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
        self.entries_from_payloads(CompilerRolePayloads::Retained(view.retained_payloads()))
            .map(|_| ())
    }

    pub(crate) fn entries_from_metadata(
        &self,
        metadata: &RetainedArtifactMetadata,
    ) -> Result<BTreeMap<ExactModuleIdentity, Arc<ArtifactEntry>>, CompileError> {
        self.entries_from_payloads(CompilerRolePayloads::PersistentMetadata(
            &metadata.artifacts,
        ))
    }

    pub(crate) fn entries_from_artifacts(
        &self,
        artifacts: &BTreeMap<ArtifactId, Arc<ArtifactEntry>>,
    ) -> Result<BTreeMap<ExactModuleIdentity, Arc<ArtifactEntry>>, CompileError> {
        self.entries_from_payloads(CompilerRolePayloads::Metadata(artifacts))
    }

    fn entries_from_payloads(
        &self,
        artifacts: CompilerRolePayloads<'_>,
    ) -> Result<BTreeMap<ExactModuleIdentity, Arc<ArtifactEntry>>, CompileError> {
        let selected = self.associated_entries(&artifacts)?;
        self.validate_requirements(selected.values().map(Arc::as_ref), |owner| {
            artifacts
                .get(&self.roles[owner].interface())
                .map(Arc::as_ref)
        })?;
        Ok(selected)
    }
    fn validate_requirements<'a>(
        &self,
        entries: impl IntoIterator<Item = &'a ArtifactEntry>,
        interface: impl Fn(&ExactModuleIdentity) -> Option<&'a ArtifactEntry>,
    ) -> Result<(), CompileError> {
        for entry in entries {
            for required in &entry.requirements {
                self.roles.get(required).ok_or_else(|| {
                    admission_failure(ArtifactInventoryFailure::MissingDependency {
                        artifact: entry.descriptor.id,
                        dependent: entry.descriptor.owner.clone(),
                        required: required.clone(),
                        dependency: ArtifactDependency::Interface,
                    })
                })?;
                if entry.interface_seals.get(required).is_some_and(|seal| {
                    interface(required)
                        .expect("associated canonical interface")
                        .descriptor
                        .interface_sha256
                        != *seal
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
        Ok(())
    }

    /// Acquired rows retain their authentication when chosen whole. Only the
    /// dependency relationship can change when independently admitted sets join.
    fn validate_selected_requirements(
        &self,
        selected: &BTreeMap<ExactModuleIdentity, ResolvedCompilerRole>,
    ) -> Result<(), CompileError> {
        self.validate_requirements(
            selected
                .values()
                .map(|row| row.original().unwrap_or(row.interface()).1.as_ref()),
            |owner| selected.get(owner).map(|row| row.interface().1.as_ref()),
        )
    }

    pub(crate) fn validate_associations(&self, view: &ArtifactView) -> Result<(), CompileError> {
        self.resolve_associations(view).map(|_| ())
    }

    fn resolve_associations(
        &self,
        view: &ArtifactView,
    ) -> Result<BTreeMap<ExactModuleIdentity, ResolvedCompilerRole>, CompileError> {
        let artifacts = CompilerRolePayloads::Retained(view.retained_payloads());
        let state = view.lease.inventory.0.lock().expect("inventory lock");
        let owned = &view.read_projection(&state).nodes;
        self.resolve_associations_from(&artifacts, &state, |id| view.resolve_binding(id, owned))
    }

    fn resolve_associations_from(
        &self,
        artifacts: &CompilerRolePayloads<'_>,
        state: &InventoryState,
        resolve: impl Fn(ArtifactId) -> Result<ArtifactBinding, CompileError>,
    ) -> Result<BTreeMap<ExactModuleIdentity, ResolvedCompilerRole>, CompileError> {
        let entries = self.associated_entries(artifacts)?;
        let mut selected = BTreeMap::new();
        for (owner, role) in &self.roles {
            #[cfg(test)]
            type_query_work::resolved_row();
            let canonical = resolve(role.interface())?;
            let original = if let Some(original) = role.original() {
                let carrier = resolve(original)?;
                ResolvedCompilerRole::validate_original_pair(&state, canonical, carrier)?;
                Some((carrier, Arc::clone(&entries[owner])))
            } else {
                None
            };
            selected.insert(
                owner.clone(),
                ResolvedCompilerRole::from_issued_bindings(
                    (
                        canonical,
                        Arc::clone(
                            artifacts
                                .get(&role.interface())
                                .expect("associated canonical interface"),
                        ),
                    ),
                    original,
                ),
            );
        }
        Ok(selected)
    }

    fn associated_entries(
        &self,
        artifacts: &CompilerRolePayloads<'_>,
    ) -> Result<BTreeMap<ExactModuleIdentity, Arc<ArtifactEntry>>, CompileError> {
        let mut selected = BTreeMap::new();
        for (owner, role) in &self.roles {
            #[cfg(test)]
            type_query_work::canonical_check();
            let interface = artifacts
                .get(&role.interface())
                .ok_or_else(|| failure("compiler interface role is outside retained custody"))?;
            let entry =
                Self::validate_role_payload(owner, role, interface, |id| artifacts.get(&id))?;
            selected.insert(owner.clone(), Arc::clone(entry));
        }
        Ok(selected)
    }
    fn validate_role_payload<'a>(
        owner: &ExactModuleIdentity,
        role: &CompilerInputRole,
        interface: &'a Arc<ArtifactEntry>,
        original: impl FnOnce(ArtifactId) -> Option<&'a Arc<ArtifactEntry>>,
    ) -> Result<&'a Arc<ArtifactEntry>, CompileError> {
        if &interface.descriptor.owner != owner || !interface.is_compiler_interface() {
            return Err(failure("compiler interface role has another owner or kind"));
        }
        match role.original() {
            None => Ok(interface),
            Some(id) => {
                let original = original(id).ok_or_else(|| {
                    failure("compiler original offer is outside retained custody")
                })?;
                let ArtifactPayload::Original(product) = &original.payload else {
                    return Err(failure("compiler original offer has no native proof"));
                };
                let canonical = product.module_interface();
                if &original.descriptor.owner != owner || {
                    #[cfg(test)]
                    type_query_work::canonical_descriptor_rebuild();
                    ArtifactEntry::canonical_artifact_id(canonical) != role.interface()
                } {
                    return Err(failure(
                        "compiler original offer differs from its interface",
                    ));
                }
                Ok(original)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    impl AcquiredNativeClosures {
        /// Observe real issued owners. The independent reference model retains
        /// pointers, so equal immutable records cannot erase acquisition choices.
        pub(crate) fn assert_collection_history(
            owners: &[Arc<crate::linked_native::AcquiredNativeClosure>],
        ) {
            assert!(
                !owners.is_empty(),
                "actual acquired owner census is nonempty"
            );
            assert!(owners.iter().all(|owner| owner.acquired().len() > 0));
            let mut home_demands = 0;
            for reverse in [false, true] {
                let mut order = owners.to_vec();
                if reverse {
                    order.reverse();
                }
                let mut actual = Self::default();
                let mut expected = Vec::new();
                for owner in &order {
                    actual = actual
                        .append(&Self::one(Arc::clone(owner)).unwrap())
                        .unwrap();
                    if !expected.iter().any(|prior| Arc::ptr_eq(prior, owner)) {
                        expected.push(Arc::clone(owner));
                    }
                    assert_eq!(actual.iter().count(), expected.len());
                    assert!(expected.iter().all(|owner| actual.contains_owner(owner)));
                    let repeated = actual.append(&actual).unwrap();
                    assert!(actual.owners.ptr_eq(&repeated.owners));
                    assert!(actual.components.ptr_eq(&repeated.components));
                    assert!(actual.recorded.ptr_eq(&repeated.recorded));
                    assert!(actual.packages.ptr_eq(&repeated.packages));
                    assert!(actual.same_held_owners(&repeated));
                }
                let replay = order.iter().rev().fold(Self::default(), |prior, owner| {
                    prior
                        .append(&Self::one(Arc::clone(owner)).unwrap())
                        .unwrap()
                });
                assert!(actual.same_held_owners(&replay));
                assert_eq!(actual.components, replay.components);
                assert_eq!(actual.recorded, replay.recorded);
                let operation =
                    tidepool_repr::execution_schema::InventoryOperation::new(Default::default());
                for owner in &order {
                    for acquired in owner.acquired() {
                        if let crate::linked_native::NativeCodeGrant::Home {
                            binding,
                            product,
                            ..
                        } = acquired.grant()
                        {
                            home_demands += 1;
                            let selected = actual
                                .group_owner(binding, &operation)
                                .unwrap()
                                .expect("real held HOME demand remains available");
                            assert_eq!(selected.code().id(), acquired.code().id());
                            let crate::linked_native::NativeCodeGrant::Home {
                                product: selected_product,
                                ..
                            } = selected.grant()
                            else {
                                panic!("HOME lookup preserves its grant kind")
                            };
                            assert!(selected_product.same_issuance(product));
                        }
                    }
                }
            }
            assert!(
                home_demands > 0,
                "actual dependent target exercises HOME demand lookup"
            );
        }
    }

    fn value_certificate(
        module: &str,
        requirements: Vec<ExactModuleIdentity>,
    ) -> Arc<crate::recovery_artifacts::CertifiedValueInterface> {
        let interface = crate::certified_products::fixture_module_interface(
            [2; 32],
            "unit",
            module,
            BTreeMap::new(),
        );
        Arc::new(
            crate::recovery_artifacts::CertifiedValueInterface::from_checked_compilation(
                [2; 32],
                ExactModuleIdentity {
                    unit: "unit".into(),
                    module: module.into(),
                },
                interface.interface_bytes().to_vec(),
                interface.package_imports_bytes().to_vec(),
                requirements,
            )
            .unwrap(),
        )
    }

    fn checked_value_admission(
        source: &CompilerSourceSupport,
        values: &[Arc<crate::recovery_artifacts::CertifiedValueInterface>],
    ) -> Result<CompilerSourceSupport, CompileError> {
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
        let projection = source.projection.merge(&issued)?;
        let catalog = NativeDependencyCatalog::from_source(source)?;
        let artifacts = projection.admit_interfaces_with_native_offers(
            &source.artifacts,
            entries,
            &source.native_offers,
            &catalog,
        )?;
        source.rebuild(projection, artifacts, source.native_offers.iter().cloned())
    }

    #[test]
    fn compiler_primary_issuer_rejects_crossed_acquisitions_and_canonical_pairs() {
        let entries = decision_affinity_entries();
        let inventory = ArtifactInventory::default();
        let issue = |marker: &str| {
            let mut issued = entries.clone();
            issued.push(Arc::new(crate::artifact_inventory::tests::entry(
                marker,
                &[],
            )));
            let view = inventory
                .admit_shared_with_demand(
                    &inventory.empty_view(),
                    issued,
                    NativeArtifactDemand::AllGroups,
                )
                .unwrap();
            CompilerSourceSupport::from_selected_issuance(
                CompilerInputProjection::from_issued_entries(&entries).unwrap(),
                view,
            )
            .unwrap()
        };
        let first = issue("PrimaryFirst");
        let second = issue("PrimarySecond");
        let owner = &entries[1].descriptor.owner;
        let row = &first.selected[owner];
        let foreign = &second.selected[owner];
        assert_ne!(row.original().unwrap().0, foreign.original().unwrap().0);
        let primary = first.compiler_primary_input(owner).unwrap().unwrap();
        assert_eq!(primary.original(), Some(row.original().unwrap().0));
        assert!(Arc::ptr_eq(primary.entry(), &row.original().unwrap().1));
        assert!(first.issue_compiler_primary_input(owner, foreign).is_err());
        assert!(second.issue_compiler_primary_input(owner, row).is_err());
        let wrong_canonical = ResolvedCompilerRole::from_issued_bindings(
            first.selected[&entries[2].descriptor.owner]
                .interface()
                .clone(),
            row.original().cloned(),
        );
        assert!(first
            .issue_compiler_primary_input(owner, &wrong_canonical)
            .is_err());
        assert!(first
            .issue_compiler_primary_input(&entries[2].descriptor.owner, row)
            .is_err());
        assert_eq!(
            first.compiler_primary_inputs().count(),
            first.selected.rows.len()
        );
    }

    #[test]
    fn demanded_compiler_projection_visits_only_issued_role_closure() {
        for unrelated in [0, 64, 256] {
            let inventory = ArtifactInventory::default();
            let seed = Arc::new(crate::artifact_inventory::tests::entry(
                "Seed",
                &["Required"],
            ));
            let required = Arc::new(crate::artifact_inventory::tests::entry(
                "Required",
                &["Leaf"],
            ));
            let leaf = Arc::new(crate::artifact_inventory::tests::entry("Leaf", &[]));
            let expected = [&seed, &required, &leaf]
                .into_iter()
                .map(|entry| (entry.descriptor.owner.clone(), Arc::clone(entry)))
                .collect::<BTreeMap<_, _>>();
            let mut entries = expected.values().cloned().collect::<Vec<_>>();
            entries.extend((0..unrelated).map(|index| {
                Arc::new(crate::artifact_inventory::tests::entry(
                    &format!("Unused{index}"),
                    &[],
                ))
            }));
            let projection = CompilerInputProjection::from_issued_entries(&entries).unwrap();
            let view = inventory
                .admit_shared(&inventory.empty_view(), entries)
                .unwrap();
            let context =
                crate::declaration_context::ExactDeclarationContext::from_authenticated_interfaces(
                    [2; 32], &view, projection,
                )
                .unwrap();
            let inputs = context
                .capture_world_primary_inputs([2; 32])
                .unwrap()
                .inputs()
                .clone();
            let support = inputs.shared_compiler_source_support();
            let roots = BTreeSet::from([seed.descriptor.owner.clone()]);
            let retained = Arc::clone(&inputs.metadata().retained);
            let operation =
                tidepool_repr::execution_schema::InventoryOperation::new(Default::default());
            let before = inventory.metrics();
            type_query_work::take();
            let demanded = inputs
                .demanded_compiler_projection(&roots, &operation)
                .unwrap();
            let work = type_query_work::take();
            let after = inventory.metrics();
            assert_eq!(
                demanded.metadata().entries,
                expected
                    .iter()
                    .map(|(k, v)| (k.clone(), Arc::clone(v)))
                    .collect()
            );
            assert_eq!(
                demanded.owners(),
                &expected.keys().cloned().collect::<BTreeSet<_>>()
            );
            assert!(Arc::ptr_eq(demanded.source_support(), &support));
            assert!(Arc::ptr_eq(&demanded.metadata().retained, &retained));
            for owner in expected.keys() {
                assert!(Arc::ptr_eq(
                    &demanded.metadata().entries[owner],
                    &support.selected[owner].interface().1,
                ));
                assert_eq!(
                    demanded.projection().snapshot_roles().get(owner),
                    support.projection().snapshot_roles().get(owner)
                );
            }
            assert_eq!(demanded.work().owner_lookups, 6);
            assert_eq!(demanded.work().payload_lookups, 3);
            assert_eq!(demanded.work().canonical_requirement_edges, 2);
            assert_eq!(demanded.work().original_requirement_edges, 0);
            assert_eq!(demanded.work().selected_rows, 3);
            assert_eq!(demanded.work().metadata_entries, 3);
            assert_eq!(work.lookups, 3);
            assert_eq!(work.cloned_rows, 3);
            assert_eq!(work.iterated_rows, 0);
            assert_eq!(work.census_rows, 0);
            assert_eq!(work.resolved_rows, 0);
            assert_eq!(work.canonical_checks, 0);
            assert_eq!(work.certificate_hashes, 0);
            assert_eq!(work.descriptor_serializations, 0);
            assert_eq!(before.graph_visits, after.graph_visits);
            assert_eq!(before.view_queries, after.view_queries);
            assert_eq!(before.entry_handle_copies, after.entry_handle_copies);
            assert!(operation.work_usage().unwrap().0 > 0);

            let empty_operation = tidepool_repr::execution_schema::InventoryOperation::new(
                tidepool_repr::execution_schema::InventoryDecodeLimits {
                    max_work: 0,
                    ..Default::default()
                },
            );
            type_query_work::take();
            let empty = inputs
                .demanded_compiler_projection(&BTreeSet::new(), &empty_operation)
                .unwrap();
            assert_eq!(empty.work(), DemandCompilerProjectionWork::default());
            assert!(empty.metadata().entries.is_empty());
            assert!(empty.owners().is_empty());
            assert_eq!(type_query_work::take(), type_query_work::Counts::default());
            assert_eq!(empty_operation.work_usage().unwrap(), (0, 0));

            let exhausted = tidepool_repr::execution_schema::InventoryOperation::new(
                tidepool_repr::execution_schema::InventoryDecodeLimits {
                    max_work: 1,
                    ..Default::default()
                },
            );
            exhausted.charge(1).unwrap();
            type_query_work::take();
            assert!(inputs
                .demanded_compiler_projection(&roots, &exhausted)
                .is_err());
            assert_eq!(type_query_work::take(), type_query_work::Counts::default());
            let missing = BTreeSet::from([ExactModuleIdentity {
                unit: "unit".into(),
                module: "Missing".into(),
            }]);
            assert!(inputs
                .demanded_compiler_projection(&missing, &operation)
                .is_err());
        }
    }

    #[test]
    fn type_delta_demand_uses_paired_metadata_without_a_cold_read() {
        use tidepool_repr::execution_schema::{InventoryDecodeLimits, InventoryOperation};
        for width in [1, 32, 128] {
            let inventory = ArtifactInventory::default();
            let entries = (0..width)
                .map(|index| {
                    Arc::new(crate::artifact_inventory::tests::entry(
                        &format!("Baseline{index}"),
                        &[],
                    ))
                })
                .collect::<Vec<_>>();
            let roles = CompilerInputProjection::from_issued_entries(&entries).unwrap();
            let view = inventory
                .admit_shared(&inventory.empty_view(), entries)
                .unwrap();
            let context =
                crate::declaration_context::ExactDeclarationContext::from_authenticated_interfaces(
                    [2; 32], &view, roles,
                )
                .unwrap();
            let original = context.capture_world_primary_inputs([2; 32]).unwrap();
            let entry = Arc::new(crate::artifact_inventory::tests::entry("AddedType", &[]));
            let owner = entry.descriptor.owner.clone();
            let incoming_inventory = ArtifactInventory::default();
            let roles =
                CompilerInputProjection::from_issued_entries(&[Arc::clone(&entry)]).unwrap();
            let incoming_view = incoming_inventory
                .admit_shared(&incoming_inventory.empty_view(), vec![entry])
                .unwrap();
            let incoming = Arc::new(
                crate::declaration_context::ExactDeclarationContext::from_authenticated_interfaces(
                    [2; 32],
                    &incoming_view,
                    roles,
                )
                .unwrap(),
            );
            let signature = type_signature("unit", "AddedType");
            let incoming = Arc::new(
                incoming
                    .select_original_type_interfaces(&[], &[&signature])
                    .unwrap(),
            );
            let changed = original.with_type_interfaces(&[incoming]).unwrap();
            let artifacts = changed.primary.inputs().artifacts();
            assert!(artifacts.reads.read_projection.get().is_none());
            let observed_inventory = artifacts.inventory();
            let before = observed_inventory.metrics();
            let empty_budget = InventoryOperation::new(InventoryDecodeLimits {
                max_work: 0,
                ..Default::default()
            });
            let empty = changed
                .primary
                .inputs()
                .demanded_compiler_projection(&BTreeSet::new(), &empty_budget)
                .unwrap();
            assert!(empty.owners().is_empty());
            assert_eq!(empty.work(), DemandCompilerProjectionWork::default());
            let empty_native = changed
                .primary
                .inputs()
                .demanded_native_inputs(empty, &BTreeSet::new(), &empty_budget)
                .unwrap();
            assert!(empty_native
                .native_availability()
                .persistent_facts()
                .is_empty());
            assert_eq!(empty_budget.work_usage().unwrap(), (0, 0));
            let operation = InventoryOperation::new(Default::default());
            let demanded = changed
                .primary
                .inputs()
                .demanded_compiler_projection(&BTreeSet::from([owner.clone()]), &operation)
                .unwrap();
            assert_eq!(demanded.owners(), &BTreeSet::from([owner]));
            assert_eq!(demanded.work().selected_rows, 1);
            assert_eq!(demanded.work().payload_lookups, 1);
            assert!(Arc::ptr_eq(
                &demanded.metadata().retained,
                &changed.primary.inputs().metadata().retained
            ));
            let selected = changed
                .primary
                .inputs()
                .demanded_native_inputs(demanded, &BTreeSet::new(), &operation)
                .unwrap();
            assert!(selected.native_availability().persistent_facts().is_empty());
            let after = observed_inventory.metrics();
            assert_eq!(before.graph_visits, after.graph_visits);
            assert_eq!(before.view_queries, after.view_queries);
            assert_eq!(before.entry_handle_copies, after.entry_handle_copies);
            assert!(artifacts.reads.read_projection.get().is_none());
            // Recompute from the actual immutable lease through a fresh read
            // owner, independently of the maintained metadata and hot demand.
            let cold = ArtifactView::from_custody(Arc::clone(&artifacts.lease)).metadata_snapshot();
            let maintained = &changed.primary.inputs().metadata().retained;
            assert!(!Arc::ptr_eq(&cold, maintained));
            assert_eq!(cold.entries, maintained.entries);
            assert_eq!(cold.artifacts, maintained.artifacts);
            assert_eq!(cold.dependencies, maintained.dependencies);
            assert_eq!(
                cold.interface_representatives,
                maintained.interface_representatives
            );
            assert_eq!(
                cold.selected_native_groups,
                maintained.selected_native_groups
            );
            assert_eq!(
                cold.ambiguous_native_owners,
                maintained.ambiguous_native_owners
            );
        }
    }

    #[test]
    fn demanded_compiler_projection_keeps_exact_native_issuer() {
        for demand in [
            NativeArtifactDemand::AllGroups,
            NativeArtifactDemand::ScopeInterfaces,
        ] {
            let entries = decision_affinity_entries();
            let inventory = ArtifactInventory::default();
            let view = inventory
                .admit_shared_with_demand(&inventory.empty_view(), entries.clone(), demand)
                .unwrap();
            let projection = CompilerInputProjection::from_issued_entries(&entries).unwrap();
            let root = entries.last().unwrap().descriptor.owner.clone();
            let lexical = entries
                .iter()
                .map(|entry| {
                    (
                        entry.descriptor.owner.clone(),
                        crate::declaration_join::ExactLexicalNode {
                            owner: entry.descriptor.owner.clone(),
                            imports: entry.requirements.clone(),
                        },
                    )
                })
                .collect::<BTreeMap<_, _>>();
            let context =
                crate::declaration_context::ExactDeclarationContext::retained_execution_fixture(
                    [2; 32],
                    &view,
                    projection,
                    lexical.into_values().collect(),
                    root.clone(),
                    &[],
                )
                .unwrap();
            let inputs = context
                .capture_world_primary_inputs([2; 32])
                .unwrap()
                .inputs()
                .clone();
            let support = inputs.shared_compiler_source_support();
            let retained = Arc::clone(&inputs.metadata().retained);
            // Positive control: the original validation path really rebuilds
            // descriptors, so zero below cannot be a disconnected observation.
            type_query_work::take();
            support.projection.entries_from_metadata(&retained).unwrap();
            let repeated = type_query_work::take();
            assert!(repeated.certificate_hashes > 0);
            assert!(repeated.descriptor_serializations > 0);
            let operation =
                tidepool_repr::execution_schema::InventoryOperation::new(Default::default());
            let demanded = inputs
                .demanded_compiler_projection(&BTreeSet::from([root.clone()]), &operation)
                .unwrap();
            let work = type_query_work::take();
            assert_eq!(work.canonical_checks, 0);
            assert_eq!(work.certificate_hashes, 0);
            assert_eq!(work.descriptor_serializations, 0);
            assert_eq!(work.iterated_rows, 0);
            assert_eq!(work.resolved_rows, 0);
            assert!(Arc::ptr_eq(demanded.source_support(), &support));
            assert_eq!(
                demanded.projection().snapshot_roles().get(&root),
                support.projection().snapshot_roles().get(&root)
            );
            assert!(demanded.metadata().entries[&root].is_native());
            assert!(!demanded.source_support().native_offers().is_empty());
            assert_eq!(
                demanded.source_support().input_decisions().recorded_facts(),
                support.input_decisions().recorded_facts()
            );
            assert_eq!(
                demanded.source_support().native_availability(),
                support.native_availability()
            );
        }
    }

    proptest::proptest! {
        #![proptest_config(proptest::test_runner::Config::with_cases(24))]
        #[test]
        fn value_publication_forks_share_unchanged_selected_rows(
            unrelated in 128usize..513,
            history in proptest::collection::vec((0usize..8, proptest::bool::ANY), 1..9),
        ) {
            let inventory = ArtifactInventory::default();
            let entries = (0..unrelated).map(|index| {
                Arc::new(crate::artifact_inventory::tests::entry(&format!("Initial{index}"), &[]))
            }).collect::<Vec<_>>();
            let expected = entries.iter().map(|entry| {
                (entry.descriptor.owner.clone(), entry.descriptor.id)
            }).collect::<BTreeMap<_, _>>();
            let projection = CompilerInputProjection::from_issued_entries(&entries).unwrap();
            let artifacts = inventory.admit_shared(&inventory.empty_view(), entries).unwrap();
            let support = CompilerSourceSupport::new(projection, artifacts, [], Default::default()).unwrap();
            let mut worlds = vec![(support, expected)];
            for (step, (parent, drop_parent)) in history.into_iter().enumerate() {
                let parent = parent % worlds.len();
                let value = value_certificate(&format!("Published{step}"), vec![]);
                let owner = ExactModuleIdentity { unit: "unit".into(), module: format!("Published{step}") };
                let (base, expected) = &worlds[parent];
                type_query_work::take();
                let fork = base.clone();
                let fork_work = type_query_work::take();
                proptest::prop_assert_eq!(fork_work.cloned_rows, 0);
                let next = fork.admit_value_interfaces(&[value.clone()]).unwrap();
                drop(fork);
                let append_work = type_query_work::take();
                eprintln!("selected-row-publication unrelated={unrelated} step={step} rows={} work={append_work:?}", expected.len());
                // Count actual row clones, including the map's copy-on-write
                // nodes. A BTreeMap clone copies every prior row and fails here.
                proptest::prop_assert!(append_work.cloned_rows < expected.len());
                proptest::prop_assert!(append_work.cloned_rows <= 64);
                proptest::prop_assert_eq!(append_work.resolved_rows, 0);
                proptest::prop_assert_eq!(append_work.census_rows, 0);
                let mut expected_next = expected.clone();
                expected_next.insert(owner, value.artifact_id());
                for (owner, before) in base.selected.iter() {
                    let after = &next.selected[owner];
                    proptest::prop_assert_eq!(after.interface().0, before.interface().0);
                    proptest::prop_assert!(Arc::ptr_eq(&after.interface().1, &before.interface().1));
                    proptest::prop_assert_eq!(&after.original(), &before.original());
                }
                worlds.push((next, expected_next));
                if drop_parent {
                    worlds.remove(parent);
                }
                // This oracle comes from the issued input recipe and each
                // branch's publications, independently of map sharing.
                for (world, expected) in &worlds {
                    let selected = world.selected_interfaces().unwrap();
                    proptest::prop_assert_eq!(selected.iter().map(|(owner, entry)| {
                        (owner.clone(), entry.descriptor.id)
                    }).collect::<BTreeMap<_, _>>(), expected.clone());
                    proptest::prop_assert_eq!(world.projection.snapshot_roles().len(), expected.len());
                    proptest::prop_assert!(world.artifacts.selected_native_groups().is_empty());
                    proptest::prop_assert_eq!(world.native_availability.rows().count(), 0);
                }
            }
        }
    }

    #[test]
    fn fresh_value_append_preserves_native_permissions_and_exact_execution_roots() {
        let raw = crate::certified_products::fixture_finalized_product(
            crate::certified_products::tests::original_groups_fixture(
                "AppendNative",
                vec![(7, vec![]), (9001, vec![])],
                61,
                &BTreeMap::new(),
            ),
            [2; 32],
        );
        let product = crate::certified_products::tests::recovered_witness_fixtures(&[raw])
            .remove(0)
            .product;
        let entry = Arc::new(ArtifactEntry::original([2; 32], product).unwrap());
        let projection = CompilerInputProjection::from_issued_entries(&[entry.clone()]).unwrap();
        let inventory = ArtifactInventory::default();
        let full = inventory
            .admit_shared(&inventory.empty_view(), vec![entry.clone()])
            .unwrap();
        let full_source =
            crate::declaration_context::issue_fixture_compiler_support(projection.clone(), full)
                .unwrap();
        let full_proof = NativeOriginalRequirement::from_selected_view(
            entry.descriptor.id,
            full_source.artifacts(),
        )
        .unwrap();
        let full_offers = [full_source
            .issue_native_offer(&entry.descriptor.owner, full_proof)
            .unwrap()];
        for ordinals in [vec![7, 9001], vec![7], vec![9001], vec![]] {
            let groups = ordinals
                .iter()
                .map(|ordinal| NativeGroupKey {
                    artifact: entry.descriptor.id,
                    original_ordinal: *ordinal,
                })
                .collect();
            let chosen = inventory
                .admit_recovery_selection(&inventory.empty_view(), vec![entry.clone()], &groups)
                .unwrap();
            let source = crate::declaration_context::issue_fixture_compiler_support(
                projection.clone(),
                chosen,
            )
            .unwrap()
            .retain_offers(full_offers.iter().cloned())
            .unwrap();
            let before = source.artifacts.capture_graph_selection();
            let old_edges = source.artifacts.binding_dependencies();
            let required_binding = source.selected[&entry.descriptor.owner].interface().0;
            let value = value_certificate("FreshAppend", vec![entry.descriptor.owner.clone()]);
            let work = inventory.metrics();
            let actual = source.admit_value_interfaces(&[value.clone()]).unwrap();
            let after = inventory.metrics();
            assert_eq!(after.graph_visits, work.graph_visits);
            assert_eq!(after.admission_owner_lookups, work.admission_owner_lookups);
            let graph = actual.artifacts.capture_graph_selection();
            let expected_roots = before
                .roots
                .iter()
                .copied()
                .chain(std::iter::once(InventoryNodeKey::Artifact(
                    actual.artifacts.namespace()[&value.artifact_id()],
                )))
                .collect::<BTreeSet<_>>();
            assert_eq!(
                graph.roots.iter().copied().collect::<BTreeSet<_>>(),
                expected_roots
            );
            assert_eq!(graph.native_groups, before.native_groups);
            for binding in before.namespace {
                assert_eq!(actual.artifacts.namespace()[&binding.artifact], binding);
            }
            assert!(old_edges
                .iter()
                .all(|edge| actual.artifacts.binding_dependencies().contains(edge)));
            let expected_new_edge = (
                InventoryNodeKey::Artifact(actual.artifacts.namespace()[&value.artifact_id()]),
                InventoryNodeKey::Artifact(required_binding),
                ArtifactDependency::Interface,
            );
            assert_eq!(
                actual
                    .artifacts
                    .binding_dependencies()
                    .into_iter()
                    .filter(|edge| !old_edges.contains(edge))
                    .collect::<BTreeSet<_>>(),
                BTreeSet::from([expected_new_edge]),
                "a fresh type adds only its canonical interface obligation, never an original or Group edge",
            );
            assert!(actual
                .native_availability
                .same_recorded_facts(&source.native_availability));
            let row = actual
                .native_availability
                .rows()
                .find(|row| {
                    row.target_original(row.original_owner())
                        .is_some_and(|original| original.descriptor.id == entry.descriptor.id)
                })
                .unwrap();
            assert_eq!(row.permits(7), ordinals.contains(&7));
            assert_eq!(row.permits(9001), ordinals.contains(&9001));
            assert_eq!(
                actual.input_decisions.recorded_facts(),
                source.input_decisions.recorded_facts()
            );
            assert!(Arc::ptr_eq(
                &actual.selected.home_units,
                &source.selected.home_units
            ));
            let oracle = checked_value_admission(&source, &[value.clone()]).unwrap();
            assert_eq!(actual.projection, oracle.projection);
            assert!(actual
                .native_availability
                .same_recorded_facts(&oracle.native_availability));
            assert_eq!(
                graph.native_groups,
                oracle.artifacts.capture_graph_selection().native_groups
            );
            let repeated = actual.admit_value_interfaces(&[value]).unwrap();
            assert!(Arc::ptr_eq(
                &actual.artifacts.lease,
                &repeated.artifacts.lease
            ));
            assert!(actual.selected.same_recorded_facts(&repeated.selected));
            let empty = actual.admit_value_interfaces(&[]).unwrap();
            assert!(Arc::ptr_eq(&actual.selected, &empty.selected));
        }
    }

    proptest::proptest! {
        #![proptest_config(proptest::test_runner::Config::with_cases(24))]
        #[test]
        fn value_repeat_requires_exact_dependency_issuer(extra in 1usize..5) {
            let inventory = ArtifactInventory::default();
            let canonical = Arc::new(crate::artifact_inventory::tests::entry("SharedCanonical", &[]));
            let make = |extra: usize| {
                let mut entries = vec![canonical.clone()];
                entries.extend((0..extra).map(|index| Arc::new(
                    crate::artifact_inventory::tests::entry(&format!("Cohort{index}"), &[]),
                )));
                let view = inventory.admit_shared(&inventory.empty_view(), entries).unwrap();
                CompilerSourceSupport::new(
                    CompilerInputProjection::from_issued_entries(&[canonical.clone()]).unwrap(),
                    view, [], Default::default(),
                ).unwrap()
            };
            let first = make(0);
            let value = value_certificate("IssuedValue", vec![canonical.descriptor.owner.clone()]);
            let first = first.admit_value_interfaces(&[value.clone()]).unwrap();
            let second = make(extra);
            let a = first.selected[&canonical.descriptor.owner].interface().0;
            let b = second.selected[&canonical.descriptor.owner].interface().0;
            proptest::prop_assert_eq!(a.artifact, b.artifact);
            proptest::prop_assert_ne!(a, b);
            let mixed = second.merge(&first).unwrap();
            let old_value = mixed.selected[&ExactModuleIdentity { unit: "unit".into(), module: "IssuedValue".into() }].interface().0;
            let old_edge = (InventoryNodeKey::Artifact(old_value), InventoryNodeKey::Artifact(a), ArtifactDependency::Interface);
            proptest::prop_assert!(mixed.artifacts.binding_dependencies().contains(&old_edge));
            let entry = Arc::new(ArtifactEntry::interface(value.interface().clone(), JoinedInterfaceRole::ValueInterface, value.requirements().to_vec()));
            proptest::prop_assert!(matches!(inventory.prepare_value_interface_append(&mixed, vec![entry]).unwrap(),
                ValueInterfaceAdmission::General { reason: ValueInterfaceGeneralAdmission::DifferentRequirementBindings(_), .. }),
                "repeating a value must check the exact requirement issuer");
            let repeated = mixed.admit_value_interfaces(&[value.clone()]).unwrap();
            let oracle = checked_value_admission(&mixed, &[value]).unwrap();
            proptest::prop_assert_eq!(repeated.artifacts.capture_graph_selection(), oracle.artifacts.capture_graph_selection());
            let unrelated = value_certificate("UnrelatedFreshValue", vec![]);
            let appended = mixed.admit_value_interfaces(&[unrelated]).unwrap();
            proptest::prop_assert_eq!(appended.selected[&canonical.descriptor.owner].interface().0, b);
            proptest::prop_assert!(appended.artifacts.binding_dependencies().contains(&old_edge));
            proptest::prop_assert_eq!(appended.artifacts.selected_native_groups(), mixed.artifacts.selected_native_groups());
        }
    }

    #[test]
    fn value_append_refuses_bad_metadata_and_keeps_hidden_owner_checks() {
        let inventory = ArtifactInventory::default();
        let required = Arc::new(crate::artifact_inventory::tests::entry(
            "RequiredValueType",
            &[],
        ));
        let hidden = Arc::new(crate::artifact_inventory::tests::entry(
            "HiddenValueType",
            &[],
        ));
        let view = inventory
            .admit_shared(
                &inventory.empty_view(),
                vec![required.clone(), hidden.clone()],
            )
            .unwrap();
        let source = CompilerSourceSupport::new(
            CompilerInputProjection::from_issued_entries(&[required.clone()]).unwrap(),
            view,
            [],
            Default::default(),
        )
        .unwrap();
        let hidden_value = value_certificate("HiddenValueType", vec![]);
        let hidden_entry = Arc::new(ArtifactEntry::interface(
            hidden_value.interface().clone(),
            JoinedInterfaceRole::ValueInterface,
            vec![],
        ));
        assert!(matches!(
            inventory
                .prepare_value_interface_append(&source, vec![hidden_entry])
                .unwrap(),
            ValueInterfaceAdmission::General {
                reason: ValueInterfaceGeneralAdmission::RetainedOwner(_),
                ..
            }
        ));
        let value = value_certificate("CheckedValue", vec![required.descriptor.owner.clone()]);
        let first = source.admit_value_interfaces(&[value.clone()]).unwrap();
        let bad_requirements = value_certificate("CheckedValue", vec![]);
        assert_eq!(value.artifact_id(), bad_requirements.artifact_id());
        assert!(
            matches!(first.admit_value_interfaces(&[bad_requirements]), Err(CompileError::ArtifactInventory(error))
            if matches!(error.failure, ArtifactInventoryFailure::MetadataConflict { .. }))
        );
        let absent = value_certificate(
            "AbsentRequirement",
            vec![ExactModuleIdentity {
                unit: "unit".into(),
                module: "Absent".into(),
            }],
        );
        assert!(
            matches!(source.admit_value_interfaces(&[absent]), Err(CompileError::ArtifactInventory(error))
            if matches!(error.failure, ArtifactInventoryFailure::MissingDependency { .. }))
        );
        let foreign = value_certificate(
            "ForeignRequirement",
            vec![ExactModuleIdentity {
                unit: "foreign".into(),
                module: required.descriptor.owner.module.clone(),
            }],
        );
        assert!(
            matches!(source.admit_value_interfaces(&[foreign]), Err(CompileError::ArtifactInventory(error))
            if matches!(error.failure, ArtifactInventoryFailure::MissingDependency { .. }))
        );
        let replacement = value_certificate("RequiredValueType", vec![]);
        assert!(
            matches!(source.admit_value_interfaces(&[replacement]), Err(CompileError::ArtifactInventory(error))
            if matches!(error.failure, ArtifactInventoryFailure::OwnerConflict { .. }))
        );
        let mut sealed = ArtifactEntry::interface(
            value.interface().clone(),
            JoinedInterfaceRole::ValueInterface,
            value.requirements().to_vec(),
        );
        sealed
            .interface_seals
            .insert(required.descriptor.owner.clone(), [0; 32]);
        assert!(
            matches!(inventory.prepare_value_interface_append(&source, vec![Arc::new(sealed)]), Err(CompileError::ArtifactInventory(error))
            if matches!(error.failure, ArtifactInventoryFailure::InterfaceSealMismatch { .. }))
        );
    }

    #[test]
    fn fresh_value_batch_preserves_local_cyclic_interface_requirements() {
        let source = CompilerSourceSupport::empty();
        let left = value_certificate(
            "LocalLeft",
            vec![ExactModuleIdentity {
                unit: "unit".into(),
                module: "LocalRight".into(),
            }],
        );
        let right = value_certificate(
            "LocalRight",
            vec![ExactModuleIdentity {
                unit: "unit".into(),
                module: "LocalLeft".into(),
            }],
        );
        let batch = [left.clone(), right.clone()];
        let actual = source.admit_value_interfaces(&batch).unwrap();
        let oracle = checked_value_admission(&source, &batch).unwrap();
        assert_eq!(actual.projection, oracle.projection);
        assert_eq!(
            actual.artifacts.capture_graph_selection(),
            oracle.artifacts.capture_graph_selection()
        );
        let namespace = actual.artifacts.namespace();
        let edges = actual.artifacts.binding_dependencies();
        assert!(edges.contains(&(
            InventoryNodeKey::Artifact(namespace[&left.artifact_id()]),
            InventoryNodeKey::Artifact(namespace[&right.artifact_id()]),
            ArtifactDependency::Interface
        )));
        assert!(edges.contains(&(
            InventoryNodeKey::Artifact(namespace[&right.artifact_id()]),
            InventoryNodeKey::Artifact(namespace[&left.artifact_id()]),
            ArtifactDependency::Interface
        )));
    }

    // The ordinary type-query front door parses Names through the existing
    // signature codec; interface authority still comes from the admitted view.
    fn type_signature(unit: &str, module: &str) -> crate::checked_cell::ExactCheckedSignature {
        use ciborium::Value;
        let text = |value: &str| Value::Text(value.to_owned());
        let wire = Value::Array(vec![
            text("TPCHECKEDSIGNATURE2"),
            text("request-reply"),
            text("input type"),
            Value::Bytes(vec![1]),
            Value::Array(vec![Value::Array(vec![
                text(unit),
                text(module),
                text("type"),
                text("Input"),
            ])]),
        ]);
        let mut bytes = Vec::new();
        ciborium::into_writer(&wire, &mut bytes).unwrap();
        crate::checked_cell::fixture_checked_signature(&bytes).unwrap()
    }

    proptest::proptest! {
        #![proptest_config(proptest::test_runner::Config::with_cases(32))]
        #[test]
        fn borrowed_type_queries_visit_only_their_required_rows(
            unrelated in 0usize..65, queries in 1usize..5,
        ) {
            use crate::declaration_context::ExactDeclarationContext;
            let inventory = ArtifactInventory::default();
            let root = Arc::new(crate::artifact_inventory::tests::entry("Selected", &["Required"]));
            let required = Arc::new(crate::artifact_inventory::tests::entry("Required", &[]));
            let mut entries = vec![Arc::clone(&root), Arc::clone(&required)];
            entries.extend((0..unrelated).map(|index| {
                Arc::new(ArtifactEntry::canonical(crate::certified_products::fixture_module_interface(
                    [2; 32], &format!("other_unit_{index}"), "Unrelated", BTreeMap::new(),
                )))
            }));
            // Expected canonical bytes and units are recomputed from the original
            // issued inputs, before any borrowed query or restricted owner exists.
            let expected_home_units = entries.iter().flat_map(|entry| match &entry.payload {
                ArtifactPayload::Canonical(interface) => interface.home_units().iter().cloned().collect::<Vec<_>>(),
                _ => unreachable!(),
            }).collect::<BTreeSet<_>>();
            let view = inventory.admit_shared(&inventory.empty_view(), entries.clone()).unwrap();
            let projection = CompilerInputProjection::from_issued_entries(&entries).unwrap();
            type_query_work::take();
            let support = CompilerSourceSupport::new(projection.clone(), view.clone(), [], Default::default()).unwrap();
            let admission_work = type_query_work::take();
            proptest::prop_assert_eq!(admission_work.census_rows, entries.len());
            proptest::prop_assert_eq!(support.selected.home_units.iter().cloned().collect::<BTreeSet<_>>(), expected_home_units);
            let context = Arc::new(ExactDeclarationContext::from_authenticated_interfaces([2; 32], &view, projection).unwrap());
            let signature = type_signature("unit", "Selected");
            type_query_work::take();
            let mut narrowed = Vec::new();
            for _ in 0..queries {
                narrowed.push(context.select_original_type_interfaces(&[], &[&signature]).unwrap());
            }
            let work = type_query_work::take();
            eprintln!("borrowed-type-query-work unrelated={unrelated} queries={queries} admission={admission_work:?} query={work:?}");
            proptest::prop_assert_eq!(work.iterated_rows, 0);
            proptest::prop_assert_eq!(work.resolved_rows, 0);
            proptest::prop_assert!(work.lookups > 0 && work.lookups <= 9 * queries);
            proptest::prop_assert_eq!(work.canonical_checks, 2 * queries);
            proptest::prop_assert_eq!(work.census_rows, 2 * queries);
            let expected = BTreeMap::from([
                (root.descriptor.owner.clone(), Arc::clone(&root)),
                (required.descriptor.owner.clone(), Arc::clone(&required)),
            ]);
            // Full validating admission is an oracle outside the measured region.
            let oracle_view = support.selected_interfaces().unwrap().select_owners(expected.keys().cloned().collect()).unwrap();
            let oracle = CompilerSourceSupport::new(
                CompilerInputProjection::from_issued_entries(&expected.values().cloned().collect::<Vec<_>>()).unwrap(),
                oracle_view, [], Default::default(),
            ).unwrap();
            for result in &narrowed {
                let actual = result.compiler_source_support();
                proptest::prop_assert!(actual.selected.same_recorded_facts(&oracle.selected));
                proptest::prop_assert_eq!(actual.artifacts.selection_snapshot(), oracle.artifacts.selection_snapshot());
                proptest::prop_assert_eq!(actual.selected.home_units.iter().cloned().collect::<BTreeSet<_>>(), BTreeSet::from(["unit".to_owned()]));
                for (owner, original) in &expected {
                    proptest::prop_assert_eq!(&actual.selected[owner].interface().1, original);
                }
                proptest::prop_assert!(result.compiler_source_support().artifacts().selected_native_groups().is_empty());

            }
            // The same Name is a missing home in the full owner, but an external
            // unit in the restricted owner. Its classification must not leak.
            if unrelated > 0 {
                let absent = type_signature("other_unit_0", "Absent");
                proptest::prop_assert!(context.select_original_type_interfaces(&[], &[&absent]).is_err());
                let external = narrowed[0].select(&[], &[&absent]).unwrap();
                proptest::prop_assert!(external.compiler_source_support().projection().roles().is_empty());
            }
            let missing_home = type_signature("unit", "Absent");
            proptest::prop_assert!(context.select_original_type_interfaces(&[], &[&missing_home]).is_err());
            // An explicit full-map observation must be visible to the row oracle;
            // reinserting the deleted wrapper loop would fail the zero above.
            type_query_work::take();
            let full = support.selected_interfaces().unwrap().iter()
                .map(|(owner, entry)| (owner.clone(), Arc::clone(entry))).collect::<BTreeMap<_, _>>();
            let observed = type_query_work::take();
            proptest::prop_assert_eq!(full.len(), entries.len());
            proptest::prop_assert_eq!(observed.iterated_rows, entries.len());
        }
    }

    #[test]
    fn closed_interface_narrowing_preserves_rows_without_output_metadata() {
        for unrelated in [1, 16, 64] {
            let inventory = ArtifactInventory::default();
            let selected = Arc::new(crate::artifact_inventory::tests::entry(
                "Selected",
                &["Required"],
            ));
            let required = Arc::new(crate::artifact_inventory::tests::entry("Required", &[]));
            let mut entries = vec![selected.clone(), required.clone()];
            entries.extend((0..unrelated).map(|index| {
                Arc::new(crate::artifact_inventory::tests::entry(
                    &format!("Unrelated{index}"),
                    &[],
                ))
            }));
            let view = inventory
                .admit_shared(&inventory.empty_view(), entries)
                .unwrap();
            // Expected bindings and bytes come from the original issued view,
            // before either narrowing implementation is called.
            let expected_issued = [&selected, &required]
                .into_iter()
                .map(|entry| {
                    let ArtifactPayload::Canonical(interface) = &entry.payload else {
                        panic!("canonical fixture")
                    };
                    (
                        entry.descriptor.owner.clone(),
                        (
                            view.lease.selection.namespace[&entry.descriptor.id],
                            interface.interface_bytes().to_vec(),
                        ),
                    )
                })
                .collect::<BTreeMap<_, _>>();
            let projection =
                CompilerInputProjection::from_issued_entries(&[selected.clone(), required])
                    .unwrap();
            let support =
                CompilerSourceSupport::new(projection, view, [], Default::default()).unwrap();
            let before = inventory.metrics();
            let interfaces = support.selected_interfaces().unwrap();
            let owner = selected.descriptor.owner.clone();
            let narrowed = interfaces.support_for_owners(vec![owner.clone()]).unwrap();
            let candidate = inventory.metrics();
            assert!(narrowed
                .artifacts
                .reads
                .read_projection
                .get()
                .is_none_or(|projection| projection.metadata.get().is_none()));
            for (owner, (binding, bytes)) in &expected_issued {
                let row = &narrowed.selected[owner];
                assert_eq!(row.interface().0, *binding);
                let ArtifactPayload::Canonical(interface) = &row.interface().1.payload else {
                    panic!("canonical narrowed row")
                };
                assert_eq!(interface.interface_bytes(), bytes.as_slice());
                assert!(row.original().is_none());
            }
            // Explicit metadata observation initializes the cache only after
            // the measured narrowing has preserved its untouched state.
            narrowed.artifacts.metadata_snapshot();
            assert!(narrowed
                .artifacts
                .reads
                .read_projection
                .get()
                .is_some_and(|projection| projection.metadata.get().is_some()));
            eprintln!(
                "closed-role-work unrelated={unrelated} graph={} handles={}",
                candidate.graph_visits - before.graph_visits,
                candidate.entry_handle_copies - before.entry_handle_copies,
            );
            assert!(interfaces
                .support_for_owners(vec![ExactModuleIdentity {
                    unit: "unit".into(),
                    module: "Absent".into()
                }])
                .is_err());
            let owners = BTreeSet::from([owner]);
            let source = support.source_support(&owners).unwrap();
            let recomputed = support
                .projection
                .source_support(&support.artifacts, &owners)
                .unwrap();
            let recomputed = support
                .rebuild_narrowed(recomputed.projection, recomputed.artifacts, [])
                .unwrap();
            assert_eq!(source.projection, recomputed.projection);
            assert!(source.selected.same_recorded_facts(&recomputed.selected));
            assert_eq!(
                source.artifacts.selection_snapshot(),
                recomputed.artifacts.selection_snapshot()
            );
            assert_eq!(
                source.input_decisions.recorded_facts(),
                recomputed.input_decisions.recorded_facts()
            );
            drop(interfaces);
            drop(support);
            drop(recomputed);
            // This checks retained semantic facts, not physical materialization
            // release: the fixture still holds its original entry handle.
            assert_eq!(
                source.selected[&selected.descriptor.owner]
                    .interface()
                    .1
                    .descriptor
                    .id,
                selected.descriptor.id
            );
        }
    }

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
            let historical_source = crate::declaration_context::issue_fixture_compiler_support(
                historical_projection.clone(),
                view.clone(),
            )
            .unwrap();
            let proof = NativeOriginalRequirement::from_selected_view(
                historical.descriptor.id,
                historical_source.artifacts(),
            )
            .unwrap();
            let offer = historical_source
                .issue_native_offer(&historical.descriptor.owner, proof)
                .unwrap();
            let support = crate::declaration_context::recover_fixture_compiler_support(
                projection,
                view,
                [offer],
            )
            .unwrap();
            assert!(
                support.artifacts.reads.read_projection.get().is_none(),
                "the real offer retention must leave a cold returned view for this control"
            );
            let before = inventory.metrics();
            let view_owner = Arc::clone(&support.artifacts.lease);
            for _ in 0..4 {
                let observation = support.selected_interfaces().unwrap();
                let actual = observation
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

            assert!(support.artifacts.reads.read_projection.get().is_none());
            let before_narrowing = inventory.metrics();
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

            let after_narrowing = inventory.metrics();
            eprintln!(
                "closed-role-native-work unrelated_groups={group_count} whole_chain_graph={} whole_chain_handles={}",
                after_narrowing.graph_visits - before_narrowing.graph_visits,
                after_narrowing.entry_handle_copies - before_narrowing.entry_handle_copies
            );

            let reusable = CompilerSourceSupport::new(
                historical_projection,
                support.artifacts.clone(),
                support.native_offers.iter().cloned(),
                support.native_availability.clone(),
            )
            .unwrap();
            let original_graph = reusable.artifacts.capture_graph_selection();
            let before_policy = inventory.metrics();
            type_query_work::take();
            let interface_only = reusable.for_source_owners(&BTreeSet::new()).unwrap();
            assert_eq!(type_query_work::take().census_rows, 0);
            assert!(Arc::ptr_eq(
                &reusable.selected.home_units,
                &interface_only.selected.home_units
            ));
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

            // Real recovered native custody remains private while closed-role
            // source narrowing agrees with the independent validating route.
            let owners = BTreeSet::from([historical.descriptor.owner.clone()]);
            let narrowed = reusable.source_support(&owners).unwrap();
            let legacy = reusable
                .projection
                .source_support(&reusable.artifacts, &owners)
                .unwrap();
            let legacy = reusable
                .rebuild_narrowed(
                    legacy.projection,
                    legacy.artifacts,
                    reusable.native_offers.iter().cloned(),
                )
                .unwrap();
            assert!(narrowed.selected.same_recorded_facts(&legacy.selected));
            assert_eq!(narrowed.projection, legacy.projection);
            assert_eq!(
                narrowed.input_decisions.recorded_facts(),
                legacy.input_decisions.recorded_facts()
            );
            assert_eq!(
                narrowed.artifacts.selection_snapshot(),
                legacy.artifacts.selection_snapshot()
            );
            assert_eq!(
                narrowed.native_offer_observations().unwrap(),
                legacy.native_offer_observations().unwrap()
            );
            let expected_binding = legacy.selected[&historical.descriptor.owner].interface().0;
            drop(legacy);
            drop(reusable);
            // Retained semantic payload/canonical pairing; this fixture does
            // not establish last physical materialization release.
            let row = &narrowed.selected[&historical.descriptor.owner];
            assert_eq!(
                row.original().unwrap().1.descriptor.id,
                historical.descriptor.id
            );
            assert_eq!(row.interface().0, expected_binding);

            let missing =
                CompilerInputProjection::from_issued_entries(&[Arc::clone(&selected)]).unwrap();
            let refusal = CompilerSourceSupport::new(
                missing,
                support.artifacts.clone(),
                [],
                Default::default(),
            )
            .unwrap_err();
            assert!(matches!(refusal, CompileError::ArtifactInventory(error)
                if matches!(&error.failure, ArtifactInventoryFailure::MissingDependency { dependent, required: dependency, .. }
                    if dependent == &selected.descriptor.owner && dependency == &required.descriptor.owner)));
            assert_eq!(
                support.selected_interfaces().unwrap().iter().count(),
                expected.len()
            );
        }
    }

    #[test]
    fn payload_validation_preserves_exact_membership_and_first_refusal() {
        let inventory = ArtifactInventory::default();
        let selected = Arc::new(crate::artifact_inventory::tests::entry("Selected", &[]));
        let excluded = Arc::new(crate::artifact_inventory::tests::entry("Excluded", &[]));
        let full = inventory
            .admit_shared(
                &inventory.empty_view(),
                vec![selected.clone(), excluded.clone()],
            )
            .unwrap();
        let narrowed = full.select_roots(vec![selected.descriptor.id]).unwrap();
        // Reuse the already issued narrow selection with its broad parent
        // acquisition map, exactly as the production draft owner does. This
        // changes no roots, namespace, proof, or binding authority.
        let narrowed = ArtifactView::new(ViewDraft {
            roots: vec![],
            selection: Arc::clone(&narrowed.lease.selection),
            inventory: inventory.clone(),
            parents: vec![Arc::clone(&full.lease)],
            issuing_payloads: BTreeMap::new(),
            materialization_parents: vec![],
            materialization: Mutex::new(BTreeMap::new()),
        });
        assert!(narrowed
            .lease
            .payloads
            .contains_key(&excluded.descriptor.id));
        assert!(narrowed
            .retained_payloads()
            .get(&excluded.descriptor.id)
            .is_none());
        let valid = CompilerInputProjection::from_issued_entries(&[selected.clone()]).unwrap();
        valid.validate(&narrowed).unwrap();
        valid.validate_associations(&narrowed).unwrap();
        let mut invalid = valid.clone();
        invalid.roles.insert(
            ExactModuleIdentity {
                unit: "unit".into(),
                module: "AFirst".into(),
            },
            CompilerInputRole::InterfaceOnly {
                interface: excluded.descriptor.id,
            },
        );
        invalid.roles.insert(
            ExactModuleIdentity {
                unit: "unit".into(),
                module: "ZLater".into(),
            },
            CompilerInputRole::InterfaceOnly {
                interface: selected.descriptor.id,
            },
        );
        let actual = invalid.validate(&narrowed).unwrap_err();
        let expected = invalid
            .entries_from_metadata(&narrowed.metadata_snapshot())
            .unwrap_err();
        assert_eq!(format!("{actual:?}"), format!("{expected:?}"));
        assert!(
            format!("{actual:?}").contains("compiler interface role is outside retained custody")
        );
        let actual = invalid.validate_associations(&narrowed).unwrap_err();
        assert_eq!(format!("{actual:?}"), format!("{expected:?}"));
    }

    #[test]
    fn native_offer_facts_equality_survives_reversed_live_order() {
        let entries = decision_affinity_entries();
        let projection = CompilerInputProjection::from_issued_entries(&entries).unwrap();
        let inventory = ArtifactInventory::default();
        let view = inventory
            .admit_shared(&inventory.empty_view(), entries)
            .unwrap();
        let source = CompilerSourceSupport::from_selected_issuance(projection, view).unwrap();
        assert!(source.native_offers.len() > 1);
        let reversed = source
            .rebuild(
                source.projection.clone(),
                source.artifacts.clone(),
                source.native_offers.iter().rev().cloned(),
            )
            .unwrap();
        assert_ne!(source.native_offers, reversed.native_offers);
        assert_eq!(source.native_offer_facts, reversed.native_offer_facts);
        assert_eq!(source.projection, reversed.projection);
        assert_eq!(source.artifacts, reversed.artifacts);
        assert!(source
            .native_availability
            .same_recorded_facts(&reversed.native_availability));
        assert!(source.selected.same_recorded_facts(&reversed.selected));
        assert_eq!(
            source.input_decisions.recorded_facts(),
            reversed.input_decisions.recorded_facts()
        );
        assert_eq!(source.native_fact_owner, reversed.native_fact_owner);
        crate::exact_context::take_native_fact_work();
        assert!(source.same_recorded_facts(&reversed));
        assert_eq!(
            crate::exact_context::take_native_fact_work(),
            crate::exact_context::NativeFactWork::default()
        );
    }

    #[test]
    fn completion_equivalent_wrapper_replacement_closes_all_views() {
        let entries = decision_affinity_entries();
        // A sibling inventory holds equal-by-content but pointer-distinct
        // outer wrappers around the same issued authority.
        let replacement = entries
            .iter()
            .map(|entry| Arc::new(entry.as_ref().clone()))
            .collect::<Vec<_>>();
        for (issued, clone) in entries.iter().zip(&replacement) {
            assert!(
                !Arc::ptr_eq(issued, clone),
                "fixture must mint pointer-distinct outer wrappers"
            );
        }
        let projection = CompilerInputProjection::from_issued_entries(&entries).unwrap();
        let inventory = ArtifactInventory::default();
        let view = inventory
            .admit_shared(&inventory.empty_view(), entries)
            .unwrap();
        let source =
            CompilerSourceSupport::from_selected_issuance(projection.clone(), view).unwrap();
        let sibling = ArtifactInventory::default();
        let sibling_view = sibling
            .admit_shared(&sibling.empty_view(), replacement)
            .unwrap();
        let rebuilt = source
            .rebuild(
                projection,
                sibling_view,
                source.native_offers.iter().cloned(),
            )
            .unwrap();
        for (_, row) in rebuilt.selected.iter() {
            for (binding, entry) in row.bindings() {
                assert!(
                    Arc::ptr_eq(
                        entry,
                        rebuilt
                            .artifacts
                            .lease
                            .payloads
                            .get(&binding.artifact)
                            .expect("final acquired wrapper")
                    ),
                    "closed support rows reference the final acquired wrappers"
                );
            }
        }
        // The maintained facts are exactly the maps of the closed views; a
        // caller reads the returned support directly, with no refresh step.
        let maintained = crate::exact_context::PublicationNativeFacts::maintain(
            None,
            crate::exact_context::PublicationNativeMaps {
                roles: rebuilt.projection.recorded_roles.clone(),
                offers: rebuilt.native_offer_facts.clone(),
                native_availability: rebuilt.native_availability.persistent_facts().clone(),
            },
        )
        .unwrap();
        assert_eq!(
            maintained.commitment(),
            rebuilt.native_fact_owner().commitment()
        );
        // Repeated identical enrichment reuses the exact closed state instead
        // of re-merging custody or reminting the fact owner.
        let repeated = rebuilt
            .with_acquired_native_closures_from(&rebuilt)
            .unwrap();
        assert!(Arc::ptr_eq(
            repeated.native_fact_owner(),
            rebuilt.native_fact_owner()
        ));
        assert_eq!(
            repeated.acquired_native_closures(),
            rebuilt.acquired_native_closures()
        );
    }

    #[test]
    fn ordinary_native_fact_successors_encode_only_changed_facts() {
        use crate::exact_context::{take_native_fact_work, NativeFactWork};
        for unrelated in [1, 32, 128] {
            let inventory = ArtifactInventory::default();
            let mut entries = decision_affinity_entries();
            entries.extend((0..unrelated).map(|index| {
                Arc::new(ArtifactEntry::canonical(
                    crate::certified_products::fixture_module_interface(
                        [2; 32],
                        "unrelated",
                        &format!("Prior{index}"),
                        BTreeMap::new(),
                    ),
                ))
            }));
            let projection = CompilerInputProjection::from_issued_entries(&entries).unwrap();
            let view = inventory
                .admit_shared(&inventory.empty_view(), entries)
                .unwrap();
            take_native_fact_work();
            let source = CompilerSourceSupport::from_selected_issuance(projection, view).unwrap();
            let cold = take_native_fact_work();
            assert!(
                cold.changed_entries > 0
                    && cold.serialized_bytes > 0
                    && cold.cid_validation_bytes > 0
            );
            assert!(!source.input_decisions.facts.is_empty());
            let before = Arc::clone(source.native_fact_owner());
            for _ in 0..4 {
                let request = source.close_request(None, &Default::default()).unwrap();
                assert_eq!(
                    request.input_decisions.recorded_facts(),
                    source.input_decisions.recorded_facts()
                );
                let captured = PublicationBirthFacts::capture(&request).unwrap();
                let unchanged_work = take_native_fact_work();
                assert_eq!(
                    unchanged_work,
                    NativeFactWork::default(),
                    "actual unchanged close/capture work: {unchanged_work:?}"
                );
                assert!(Arc::ptr_eq(request.native_fact_owner(), &before));
                assert!(Arc::ptr_eq(captured.facts(), &before));
            }
            // Independent actual new interface ingress, created outside measurement.
            let addition = Arc::new(ArtifactEntry::canonical(
                crate::certified_products::fixture_module_interface(
                    [2; 32],
                    "changed",
                    "OnlyNew",
                    BTreeMap::new(),
                ),
            ));
            let projection =
                CompilerInputProjection::from_issued_entries(&[Arc::clone(&addition)]).unwrap();
            let view = inventory
                .admit_shared(&inventory.empty_view(), vec![addition])
                .unwrap();
            let incoming =
                CompilerSourceSupport::new(projection, view, [], Default::default()).unwrap();
            let prior_projection = source.projection.clone();
            let prior_view = source.artifacts.capture_graph_selection();
            let expected = source.merge(&incoming).unwrap();
            take_native_fact_work();
            let inserted = source.append_program_fragment(&incoming).unwrap();
            let changed = inserted.support;
            let delta = take_native_fact_work();
            assert_eq!(inserted.promotion_qualifier_invocations, 0);
            assert_eq!(
                inserted.changed_owners,
                incoming
                    .projection
                    .roles
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>()
            );
            assert_eq!(changed.projection, expected.projection);
            assert_eq!(changed.native_offer_facts, expected.native_offer_facts);
            assert!(changed
                .native_availability
                .same_recorded_facts(&expected.native_availability));
            assert_eq!(
                changed.input_decisions.facts,
                expected.input_decisions.facts
            );
            assert_eq!(source.projection, prior_projection);
            assert_eq!(source.artifacts.capture_graph_selection(), prior_view);
            for (owner, prior) in source.selected.iter() {
                assert!(Arc::ptr_eq(
                    &changed.selected.rows[owner].interface().1,
                    &prior.interface().1
                ));
                if let Some((_, original)) = prior.original() {
                    assert!(Arc::ptr_eq(
                        &changed.selected.rows[owner].original().unwrap().1,
                        original
                    ));
                }
            }
            assert_eq!(
                delta.changed_entries, 1,
                "only one new typed role: {delta:?}"
            );
            assert!(delta.serialized_bytes > 0 && delta.serialized_bytes < cold.serialized_bytes);
            assert!(delta.link_visits > 0 && delta.unlink_visits > 0);
            assert_eq!(
                changed.native_fact_owner().offers,
                source.native_fact_owner().offers
            );
            assert_eq!(
                changed.native_fact_owner().native_availability,
                source.native_fact_owner().native_availability
            );
            assert_eq!(
                changed
                    .recorded_rows()
                    .iter()
                    .filter_map(|(owner, row)| row.native_choices().map(|choices| (owner, choices)))
                    .collect::<Vec<_>>(),
                source
                    .recorded_rows()
                    .iter()
                    .filter_map(|(owner, row)| row.native_choices().map(|choices| (owner, choices)))
                    .collect::<Vec<_>>()
            );
            assert_ne!(
                changed.native_fact_owner().commitment(),
                before.commitment()
            );
            assert_eq!(
                changed.input_decisions.recorded_facts(),
                source.input_decisions.recorded_facts()
            );
            eprintln!(
                "native-fact-successor unrelated={unrelated} cold={cold:?} changed={delta:?}"
            );
        }
    }

    #[test]
    fn program_fragment_keeps_selected_acquisitions_across_equal_witness_allocations() {
        use crate::declaration_context::NativeTransportMode;

        for distinct_witness in [false, true] {
            let receiving_entries = decision_affinity_entries();
            let incoming_entries = if distinct_witness {
                decision_affinity_entries()
            } else {
                receiving_entries
                    .iter()
                    .map(|entry| Arc::new((**entry).clone()))
                    .collect()
            };
            for (receiving, incoming) in receiving_entries.iter().zip(&incoming_entries) {
                assert!(receiving.same_recorded_facts(&incoming));
                assert!(!Arc::ptr_eq(receiving, incoming));
                let (ArtifactPayload::Original(first), ArtifactPayload::Original(second)) =
                    (&receiving.payload, &incoming.payload)
                else {
                    unreachable!()
                };
                assert_eq!(first.source_sha256(), second.source_sha256());
                assert_eq!(
                    Arc::ptr_eq(first.original_native(), second.original_native()),
                    !distinct_witness
                );
            }
            let child = receiving_entries[0].descriptor.id;
            let parent = receiving_entries[1].descriptor.id;
            let [receiving, incoming] = [
                (
                    receiving_entries.clone(),
                    "ReceivingWrapper",
                    BTreeSet::from([NativeGroupKey {
                        artifact: child,
                        original_ordinal: 9,
                    }]),
                ),
                (
                    incoming_entries,
                    "IncomingWrapper",
                    BTreeSet::from([
                        NativeGroupKey {
                            artifact: parent,
                            original_ordinal: 7,
                        },
                        NativeGroupKey {
                            artifact: child,
                            original_ordinal: 9,
                        },
                    ]),
                ),
            ]
            .map(|(entries, marker, groups)| {
                let inventory = ArtifactInventory::default();
                let base = inventory
                    .admit_shared_with_demand(
                        &inventory.empty_view(),
                        entries.clone(),
                        NativeArtifactDemand::ScopeInterfaces,
                    )
                    .unwrap();
                let view = inventory
                    .admit_recovery_selection(
                        &base,
                        vec![Arc::new(crate::artifact_inventory::tests::entry(
                            marker,
                            &[],
                        ))],
                        &groups,
                    )
                    .unwrap();
                assert_eq!(view.selected_native_groups(), groups);
                CompilerSourceSupport::from_selected_issuance(
                    CompilerInputProjection::from_issued_entries(&entries).unwrap(),
                    view,
                )
                .unwrap()
            });
            for (owner, row) in receiving.selected.iter() {
                let other = &incoming.selected[owner];
                assert_eq!(row.interface().0, other.interface().0);
                assert_eq!(row.original().unwrap().0, other.original().unwrap().0);
                assert!(!Arc::ptr_eq(
                    &row.original().unwrap().1,
                    &other.original().unwrap().1,
                ));
            }
            // Incoming offers add a different genuine contribution while the
            // receiver keeps its exact Original roles and root decisions.
            let incoming = incoming.for_source_owners(&BTreeSet::new()).unwrap();
            let old_graph = receiving.artifacts.capture_graph_selection();
            let old_decisions = receiving.input_decisions.recorded_facts().clone();
            let old_rows = receiving
                .native_availability
                .rows()
                .cloned()
                .collect::<Vec<_>>();
            let old_facts = receiving.native_availability.snapshot_facts().unwrap();
            let old_metadata = receiving
                .projection
                .project_metadata(receiving.artifacts.metadata_snapshot())
                .unwrap();
            let mut expected_facts = old_facts.clone();
            for (id, incoming) in incoming.native_availability.snapshot_facts().unwrap() {
                let previous = expected_facts.get_mut(&id).unwrap();
                assert_eq!(previous.owner, incoming.owner);
                if incoming.mode == NativeTransportMode::Full {
                    previous.mode = NativeTransportMode::Full;
                }
                for (identity, contribution) in incoming.contributions {
                    if let Some(prior) = previous
                        .contributions
                        .insert(identity, contribution.clone())
                    {
                        assert!(prior.same_recorded_facts(contribution));
                    }
                }
            }
            assert_ne!(
                expected_facts, old_facts,
                "the contribution merge cannot take its equality fast path"
            );
            let expected_offers = receiving
                .native_offers
                .iter()
                .chain(incoming.native_offers.iter())
                .map(|offer| {
                    (
                        offer.snapshot_identity().unwrap(),
                        offer.issued_group_carriers().unwrap(),
                    )
                })
                .collect::<BTreeMap<_, _>>();
            let appended = receiving.append_program_fragment(&incoming);
            let delta = appended.unwrap();
            let support = delta.support;
            let metadata = support
                .program_fragment_metadata(&old_metadata, &incoming, &delta.changed_owners)
                .unwrap();
            let cold = support
                .projection
                .project_metadata(support.artifacts.metadata_snapshot())
                .unwrap();
            assert_eq!(metadata.entries, cold.entries);
            assert_eq!(metadata.retained.artifacts, cold.retained.artifacts);
            assert_eq!(
                metadata.retained.interface_representatives,
                cold.retained.interface_representatives
            );
            assert_eq!(
                support.native_availability.snapshot_facts().unwrap(),
                expected_facts
            );
            assert_eq!(support.input_decisions.recorded_facts(), &old_decisions);
            assert_eq!(
                support.artifacts.capture_graph_selection().native_groups,
                old_graph.native_groups
            );
            assert_eq!(
                support
                    .native_offers
                    .iter()
                    .map(|offer| (
                        offer.snapshot_identity().unwrap(),
                        offer.issued_group_carriers().unwrap()
                    ))
                    .collect::<BTreeMap<_, _>>(),
                expected_offers
            );
            for (owner, row) in support.selected.iter() {
                let issued = &receiving.selected[owner];
                for ((binding, entry), (issued_binding, issued_entry)) in
                    row.bindings().zip(issued.bindings())
                {
                    assert_eq!(binding, issued_binding);
                    assert!(Arc::ptr_eq(entry, issued_entry));
                }
            }
            for row in support.native_availability.rows() {
                let issued = receiving
                    .native_availability
                    .issued_row(row.original_artifact_id())
                    .unwrap();
                assert!(Arc::ptr_eq(
                    row.target_original(row.original_owner()).unwrap(),
                    issued.target_original(issued.original_owner()).unwrap(),
                ));
            }
            let closed = support.close_request(None, &Default::default()).unwrap();
            assert_eq!(
                closed.native_availability.snapshot_facts().unwrap(),
                expected_facts
            );
            assert_eq!(closed.input_decisions.recorded_facts(), &old_decisions);
            for row in closed.native_availability.rows() {
                let id = row.original_artifact_id();
                assert!(Arc::ptr_eq(
                    row.target_original(row.original_owner()).unwrap(),
                    support
                        .native_availability
                        .issued_row(id)
                        .unwrap()
                        .target_original(row.original_owner())
                        .unwrap(),
                ));
                assert!(Arc::ptr_eq(
                    row,
                    support.native_availability.issued_row(id).unwrap()
                ));
            }
            assert_eq!(receiving.artifacts.capture_graph_selection(), old_graph);
            assert_eq!(
                receiving.native_availability.snapshot_facts().unwrap(),
                old_facts
            );
            for row in old_rows {
                assert!(Arc::ptr_eq(
                    &row,
                    receiving
                        .native_availability
                        .issued_row(row.original_artifact_id())
                        .unwrap()
                ));
            }
        }
    }

    #[test]
    fn program_original_promotion_qualifies_once_and_keeps_issued_dependencies() {
        let entries = decision_affinity_entries();
        let projection = CompilerInputProjection::from_issued_entries(&entries).unwrap();
        let inventory = ArtifactInventory::default();
        let issued = CompilerSourceSupport::from_selected_issuance(
            projection,
            inventory
                .admit_shared(&inventory.empty_view(), entries)
                .unwrap(),
        )
        .unwrap();
        let child = ExactModuleIdentity {
            unit: "fixture".into(),
            module: "AffinityChild".into(),
        };
        let ancestors = BTreeSet::from([
            ExactModuleIdentity {
                unit: "fixture".into(),
                module: "AffinityParent".into(),
            },
            ExactModuleIdentity {
                unit: "fixture".into(),
                module: "AffinityCaller".into(),
            },
        ]);
        let previous = issued.for_source_owners(&ancestors).unwrap();
        let incoming = issued
            .source_support(&BTreeSet::from([child.clone()]))
            .unwrap();
        assert!(previous.projection.roles[&child].original().is_none());
        let child_id = incoming.projection.roles[&child].original().unwrap();
        let old_rows = previous.native_availability.snapshot_facts().unwrap();
        let old_view = previous.artifacts.capture_graph_selection();
        let expected = previous.merge(&incoming).unwrap();
        let inserted = previous.append_program_fragment(&incoming).unwrap();
        assert_eq!(inserted.promotion_qualifier_invocations, 1);
        assert_eq!(inserted.support.projection, expected.projection);
        let summarize = |support: &CompilerSourceSupport| {
            support
                .native_availability
                .snapshot_facts()
                .unwrap()
                .into_iter()
                .map(|(id, fact)| {
                    (
                        id,
                        fact.mode,
                        fact.contributions.keys().copied().collect::<Vec<_>>(),
                    )
                })
                .collect::<Vec<_>>()
        };
        assert!(
            inserted
                .support
                .native_availability
                .snapshot_facts()
                .unwrap()
                == expected.native_availability.snapshot_facts().unwrap(),
            "actual={:?} expected={:?}",
            summarize(&inserted.support),
            summarize(&expected)
        );
        assert_eq!(
            previous.native_availability.snapshot_facts().unwrap(),
            old_rows
        );
        assert_eq!(previous.artifacts.capture_graph_selection(), old_view);
        assert_eq!(
            inserted.support.projection.roles[&child].original(),
            Some(child_id)
        );
        for offer in previous.native_offers.iter() {
            let retained = inserted
                .support
                .native_offers
                .iter()
                .find(|candidate| candidate.same_issuance(offer))
                .unwrap();
            assert_eq!(
                retained.issued_group_carriers().unwrap(),
                offer.issued_group_carriers().unwrap()
            );
            assert_eq!(retained.snapshot_facts(), offer.snapshot_facts());
        }
        assert!(inserted
            .support
            .native_availability
            .issued_row(child_id)
            .unwrap()
            .permits(9));
        let repeated = inserted.support.append_program_fragment(&incoming).unwrap();
        assert_eq!(repeated.promotion_qualifier_invocations, 0);
        assert_eq!(repeated.support.projection, inserted.support.projection);
    }

    #[test]
    fn program_fragment_metadata_matches_cold_multiversion_representatives_and_releases() {
        for reverse in [false, true] {
            let (metadata, previous, weak) = {
                let entries = [1, 2].map(|version| {
                    Arc::new(ArtifactEntry::original([2; 32],
                        crate::certified_products::fixture_finalized_product(
                            crate::certified_products::tests::original_groups_fixture_with_interface(
                                "MetadataBody", vec![(7, vec![])], version,
                                &BTreeMap::new(), b"same sealed interface".to_vec(),
                            ), [2; 32],
                        ),
                    ).unwrap())
                });
                let [first, second] = if reverse {
                    [entries[1].clone(), entries[0].clone()]
                } else {
                    entries.clone()
                };
                assert_ne!(first.descriptor.id, second.descriptor.id);
                let inventory = ArtifactInventory::default();
                let initial = inventory
                    .admit_shared(&inventory.empty_view(), vec![first.clone()])
                    .unwrap();
                let first_support = CompilerSourceSupport::from_selected_issuance(
                    CompilerInputProjection::from_issued_entries(&[first]).unwrap(),
                    initial,
                )
                .unwrap()
                .for_source_owners(&BTreeSet::new())
                .unwrap();
                let previous = first_support
                    .projection
                    .project_metadata(first_support.artifacts.metadata_snapshot())
                    .unwrap();
                let before = previous.retained.entries.clone();
                let before_artifacts = previous.retained.artifacts.clone();
                let expanded = inventory
                    .admit_shared(&first_support.artifacts, vec![second.clone()])
                    .unwrap();
                let incoming = CompilerSourceSupport::from_selected_issuance(
                    CompilerInputProjection::from_issued_entries(&[second.clone()]).unwrap(),
                    expanded,
                )
                .unwrap()
                .for_source_owners(&BTreeSet::new())
                .unwrap();
                let delta = first_support.append_program_fragment(&incoming).unwrap();
                assert_eq!(delta.promotion_qualifier_invocations, 0);
                let maintained = delta
                    .support
                    .program_fragment_metadata(&previous, &incoming, &delta.changed_owners)
                    .unwrap();
                let cold = delta
                    .support
                    .projection
                    .project_metadata(delta.support.artifacts.metadata_snapshot())
                    .unwrap();
                assert_eq!(maintained.entries, cold.entries);
                assert_eq!(maintained.retained.entries, cold.retained.entries);
                assert_eq!(
                    maintained.retained.interface_representatives,
                    cold.retained.interface_representatives
                );
                assert_eq!(maintained.retained.artifacts, cold.retained.artifacts);
                assert_eq!(maintained.retained.dependencies, cold.retained.dependencies);
                assert_eq!(
                    maintained.retained.ambiguous_native_owners,
                    cold.retained.ambiguous_native_owners
                );
                assert_eq!(
                    maintained.retained.selected_native_groups,
                    cold.retained.selected_native_groups
                );
                assert_eq!(previous.retained.entries, before);
                assert_eq!(previous.retained.artifacts, before_artifacts);
                assert_eq!(maintained.retained.ambiguous_native_owners.len(), 1);
                let weak = Arc::downgrade(&maintained.retained.artifacts[&second.descriptor.id]);
                (maintained, previous, weak)
            };
            assert!(
                weak.upgrade().is_some(),
                "metadata owns the actual incoming native payload"
            );
            drop(metadata);
            assert!(
                weak.upgrade().is_none(),
                "the prior snapshot must not retain a future payload"
            );
            assert_eq!(previous.retained.ambiguous_native_owners.len(), 0);
        }
    }

    #[test]
    fn keyed_type_interfaces_preserve_native_choices_and_bound_role_work() {
        let mut observed = None;
        for unrelated in [1, 32, 128] {
            let inventory = ArtifactInventory::default();
            let mut entries = decision_affinity_entries();
            entries.extend((0..unrelated).map(|index| {
                Arc::new(ArtifactEntry::canonical(
                    crate::certified_products::fixture_module_interface(
                        [2; 32],
                        "unrelated",
                        &format!("Prior{index}"),
                        BTreeMap::new(),
                    ),
                ))
            }));
            let projection = CompilerInputProjection::from_issued_entries(&entries).unwrap();
            let view = inventory
                .admit_shared(&inventory.empty_view(), entries)
                .unwrap();
            let source = CompilerSourceSupport::from_selected_issuance(projection, view).unwrap();
            let metadata = source
                .projection
                .project_metadata(source.artifacts.metadata_snapshot())
                .unwrap();

            let required = Arc::new(ArtifactEntry::canonical(
                crate::certified_products::fixture_module_interface(
                    [2; 32],
                    "changed",
                    "Required",
                    BTreeMap::new(),
                ),
            ));
            let addition = Arc::new(ArtifactEntry::canonical(
                crate::certified_products::fixture_module_interface(
                    [2; 32],
                    "changed",
                    "OnlyNew",
                    BTreeMap::from([(
                        ("changed".into(), "Required".into()),
                        required.descriptor.interface_sha256,
                    )]),
                ),
            ));
            let incoming_inventory = ArtifactInventory::default();
            let incoming_entries = vec![required, addition];
            let incoming_projection =
                CompilerInputProjection::from_issued_entries(&incoming_entries).unwrap();
            let incoming_view = incoming_inventory
                .admit_shared(&incoming_inventory.empty_view(), incoming_entries)
                .unwrap();
            let incoming = CompilerSourceSupport::new(
                incoming_projection,
                incoming_view,
                [],
                Default::default(),
            )
            .unwrap();
            let before = inventory.metrics();
            let registry_visits = inventory.0.lock().unwrap().views.len();
            type_query_work::take();
            crate::exact_context::take_native_fact_work();
            let (changed, owners) = source.with_type_interfaces(&[&incoming], [2; 32]).unwrap();
            let updated_metadata = changed.type_interface_metadata(&metadata, &owners).unwrap();
            let work = type_query_work::take();
            let native_work = crate::exact_context::take_native_fact_work();
            let after = inventory.metrics();
            assert_eq!(owners.len(), 2);
            assert_eq!(work.census_rows, 0);
            assert_eq!(work.resolved_rows, 0);
            assert_eq!(work.canonical_checks, 0);
            assert_eq!(work.certificate_hashes, 0);
            assert_eq!(work.descriptor_serializations, 0);
            assert_eq!(after.graph_visits, before.graph_visits);
            assert_eq!(after.entry_handle_copies, before.entry_handle_copies);
            assert_eq!(native_work.changed_entries, 2);
            let mut logical_work = work;
            // Persistent tree path copies clone fixed-size references; they
            // are distinct from visiting/revalidating retained role facts.
            logical_work.cloned_rows = 0;
            if let Some(expected) = observed {
                assert_eq!(logical_work, expected);
            }
            if unrelated >= 32 {
                assert!(
                    work.cloned_rows < unrelated,
                    "path reference copies must not clone the retained namespace: {work:?}"
                );
            }
            observed = Some(logical_work);
            assert!(Arc::ptr_eq(&source.native_offers, &changed.native_offers));
            assert_eq!(source.native_offer_facts, changed.native_offer_facts);
            assert!(source
                .native_availability
                .same_recorded_facts(&changed.native_availability));
            assert_eq!(
                source.input_decisions.recorded_facts(),
                changed.input_decisions.recorded_facts()
            );
            assert!(Arc::ptr_eq(
                &metadata.retained.selected_native_groups,
                &updated_metadata.retained.selected_native_groups
            ));
            assert!(Arc::ptr_eq(
                &metadata.retained.ambiguous_native_owners,
                &updated_metadata.retained.ambiguous_native_owners
            ));
            for (owner, row) in source.selected.iter() {
                assert!(changed.selected[owner].same_recorded_facts(row));
            }
            // An independent complete observation must agree with maintained
            // metadata, including hidden native custody and canonical edges.
            let complete = changed.artifacts.metadata_snapshot();
            assert_eq!(complete.as_ref().entries, updated_metadata.retained.entries);
            assert_eq!(complete.artifacts, updated_metadata.retained.artifacts);
            assert_eq!(
                complete.dependencies,
                updated_metadata.retained.dependencies
            );
            assert_eq!(
                complete.selected_native_groups,
                updated_metadata.retained.selected_native_groups
            );
            assert_eq!(
                complete.ambiguous_native_owners,
                updated_metadata.retained.ambiguous_native_owners
            );
            let oracle = source
                .extend_interfaces(&incoming.selected_interfaces().unwrap())
                .unwrap();
            assert!(changed.selected.same_recorded_facts(&oracle.selected));
            assert_eq!(changed.projection, oracle.projection);
            assert_eq!(
                changed.artifacts.capture_graph_selection(),
                oracle.artifacts.capture_graph_selection()
            );
            type_query_work::take();
            let (same, owners) = changed.with_type_interfaces(&[&incoming], [2; 32]).unwrap();
            assert!(owners.is_empty());
            assert!(Arc::ptr_eq(&same.selected, &changed.selected));
            assert!(Arc::ptr_eq(&same.artifacts.lease, &changed.artifacts.lease));
            eprintln!("keyed type delta: unrelated={unrelated}, work={work:?}, native_changes={}, residual_live_view_registry={}",
                native_work.changed_entries, registry_visits);
        }
    }

    #[test]
    fn keyed_type_interfaces_refuse_conflicting_owner_and_equal_bytes_other_binding() {
        let canonical = Arc::new(ArtifactEntry::canonical(
            crate::certified_products::fixture_module_interface(
                [2; 32],
                "type",
                "Shared",
                BTreeMap::new(),
            ),
        ));
        let extra = Arc::new(ArtifactEntry::canonical(
            crate::certified_products::fixture_module_interface(
                [2; 32],
                "type",
                "Extra",
                BTreeMap::new(),
            ),
        ));
        let issue = |entries: Vec<Arc<ArtifactEntry>>| {
            let inventory = ArtifactInventory::default();
            let projection = CompilerInputProjection::from_issued_entries(&entries).unwrap();
            let view = inventory
                .admit_shared(&inventory.empty_view(), entries)
                .unwrap();
            CompilerSourceSupport::new(projection, view, [], Default::default()).unwrap()
        };
        let source = issue(vec![Arc::clone(&canonical)]);
        let independent = issue(vec![Arc::clone(&canonical), Arc::clone(&extra)]);
        let owner = canonical.descriptor.owner.clone();
        let same_bytes = independent
            .selected_interfaces()
            .unwrap()
            .support_for_owners(vec![owner.clone()])
            .unwrap();
        assert_ne!(
            source.selected[&owner].interface().0,
            same_bytes.selected[&owner].interface().0
        );
        assert!(matches!(
            source.with_type_interfaces(&[&same_bytes], [2; 32]),
            Err(CompileError::ArtifactInventory(ArtifactInventoryError {
                failure: ArtifactInventoryFailure::BindingAmbiguity { .. },
                ..
            }))
        ));
        let different = Arc::new(ArtifactEntry::canonical(
            crate::certified_products::fixture_module_interface(
                [2; 32],
                "type",
                "Shared",
                BTreeMap::from([(
                    ("type".into(), "Extra".into()),
                    extra.descriptor.interface_sha256,
                )]),
            ),
        ));
        let conflicting = issue(vec![different, extra]);
        assert!(matches!(
            source.with_type_interfaces(&[&conflicting], [2; 32]),
            Err(CompileError::ArtifactInventory(ArtifactInventoryError {
                failure: ArtifactInventoryFailure::OwnerConflict { .. },
                ..
            }))
        ));
        let native_entries = decision_affinity_entries();
        let inventory = ArtifactInventory::default();
        let projection = CompilerInputProjection::from_issued_entries(&native_entries).unwrap();
        let native = CompilerSourceSupport::from_selected_issuance(
            projection,
            inventory
                .admit_shared(&inventory.empty_view(), native_entries)
                .unwrap(),
        )
        .unwrap();
        assert!(
            source.with_type_interfaces(&[&native], [2; 32]).is_err(),
            "type-only ingress must not import implementation roles"
        );
    }

    #[test]
    fn type_projection_and_delta_do_not_rescan_a_broad_incoming_issuer() {
        let mut semantic_work = None;
        for groups in [1, 32, 128] {
            scope_proof::take_ancestry_edge_visits();
            let product = crate::certified_products::fixture_finalized_product(
                crate::certified_products::tests::original_groups_fixture(
                    "WideTypeIssuer",
                    (0..groups).map(|ordinal| (ordinal, vec![])).collect(),
                    1,
                    &BTreeMap::new(),
                ),
                [2; 32],
            );
            let native = Arc::new(ArtifactEntry::original([2; 32], product).unwrap());
            let native_id = native.descriptor.id;
            let required = Arc::new(ArtifactEntry::canonical(
                crate::certified_products::fixture_module_interface(
                    [2; 32],
                    "types",
                    "Required",
                    BTreeMap::new(),
                ),
            ));
            let added = Arc::new(ArtifactEntry::canonical(
                crate::certified_products::fixture_module_interface(
                    [2; 32],
                    "types",
                    "Added",
                    BTreeMap::from([(
                        ("types".into(), "Required".into()),
                        required.descriptor.interface_sha256,
                    )]),
                ),
            ));
            let owner = added.descriptor.owner.clone();
            let inventory = ArtifactInventory::default();
            let entries = vec![native, required, added];
            let projection = CompilerInputProjection::from_issued_entries(&entries).unwrap();
            let view = inventory
                .admit_shared(&inventory.empty_view(), entries)
                .unwrap();
            let source = CompilerSourceSupport::from_selected_issuance(projection, view).unwrap();
            let execution = Arc::new(
                crate::declaration_context::ExactDeclarationContext::from_authenticated_interfaces(
                    [2; 32],
                    source.artifacts(),
                    source.projection.clone(),
                )
                .unwrap(),
            );
            assert!(execution
                .artifact_view()
                .entries()
                .iter()
                .any(|entry| entry.descriptor.id == native_id && entry.is_native()));
            let signature = type_signature(&owner.unit, &owner.module);
            let birth_edges = scope_proof::take_ancestry_edge_visits();
            assert!(
                birth_edges >= groups as usize,
                "the actual issuer contains the wide native graph"
            );
            let baseline_inventory = ArtifactInventory::default();
            let baseline = CompilerSourceSupport::new(
                Default::default(),
                baseline_inventory.empty_view(),
                [],
                Default::default(),
            )
            .unwrap();
            let metadata = baseline
                .projection
                .project_metadata(baseline.artifacts.metadata_snapshot())
                .unwrap();
            let before = inventory.metrics();
            type_query_work::take();
            scope_proof::take_ancestry_edge_visits();
            let projected_context = execution
                .select_original_type_interfaces(&[], &[&signature])
                .unwrap();
            let projected = projected_context.compiler_source_support();
            let (changed, owners) = baseline
                .with_type_interfaces(&[projected], [2; 32])
                .unwrap();
            let maintained = changed.type_interface_metadata(&metadata, &owners).unwrap();
            let work = type_query_work::take();
            let warm_ancestry_edges = scope_proof::take_ancestry_edge_visits();
            let after = inventory.metrics();
            assert_eq!(
                warm_ancestry_edges, 0,
                "reuse the direct-ancestor index on the same issued proof owner"
            );
            assert_eq!(after.graph_visits, before.graph_visits);
            assert_eq!(after.entry_handle_copies, before.entry_handle_copies);
            assert_eq!(projected.selected.rows.len(), 2);
            assert!(projected.native_offers.is_empty());
            assert!(projected.native_availability.rows().next().is_none());
            assert_eq!(changed.artifacts.retained_payload_handles(), 2);
            assert!(execution
                .artifact_view()
                .entries()
                .iter()
                .any(|entry| entry.descriptor.id == native_id && entry.is_native()));
            if let Some(expected) = semantic_work {
                assert_eq!(work, expected);
            }
            semantic_work = Some(work);
            let complete = changed.artifacts.metadata_snapshot();
            assert_eq!(complete.artifacts, maintained.retained.artifacts);
            assert_eq!(complete.dependencies, maintained.retained.dependencies);
            // Preserve the general native-custody projector as an independent
            // result and sensitizing cost oracle, outside the measured path.
            let mut oracle_source = source.clone();
            oracle_source.artifacts =
                ArtifactView::from_custody(Arc::clone(&source.artifacts.lease));
            let oracle_before = inventory.metrics();
            let general = oracle_source
                .selected_interfaces()
                .unwrap()
                .select_owners(projected.projection.roles.keys().cloned().collect())
                .unwrap();
            let oracle_after = inventory.metrics();
            assert!(oracle_after.graph_visits > oracle_before.graph_visits);
            assert_eq!(
                projected.artifacts.capture_graph_selection(),
                general.capture_graph_selection()
            );
            eprintln!("type projection wide issuer: groups={groups}, cold_ancestor_edges={birth_edges}, warm_ancestor_edges={warm_ancestry_edges}, work={work:?}, old_projection_graph_visits={}", oracle_after.graph_visits - oracle_before.graph_visits);
        }
    }

    #[test]
    fn complete_support_new_and_merge_do_not_materialize_unused_metadata() {
        for demand in [
            NativeArtifactDemand::AllGroups,
            NativeArtifactDemand::ScopeInterfaces,
        ] {
            let entries = decision_affinity_entries();
            let inventory = ArtifactInventory::default();
            let view = inventory
                .admit_shared_with_demand(&inventory.empty_view(), entries.clone(), demand)
                .unwrap();
            let projection = CompilerInputProjection::from_issued_entries(&entries).unwrap();
            let issued =
                CompilerSourceSupport::from_selected_issuance(projection.clone(), view).unwrap();
            assert!(issued.native_availability.rows().next().is_some());
            // A fresh read owner over the exact same issued lease prevents a
            // prior cached full snapshot from hiding eager metadata work.
            let fresh = ArtifactView::from_custody(Arc::clone(&issued.artifacts.lease));
            let mut support = CompilerSourceSupport::new(
                projection,
                fresh.clone(),
                issued.native_offers.iter().cloned(),
                issued.native_availability.clone(),
            )
            .unwrap();
            support.selected = Arc::clone(&issued.selected);
            support.input_decisions = issued.input_decisions.clone();
            let no_metadata = |view: &ArtifactView| {
                let state = view.lease.inventory.0.lock().unwrap();
                let read = view.read_projection(&state);
                assert!(read.metadata.get().is_none());
                assert!(read.dependencies.get().is_none());
                assert!(read.native_selection.get().is_none());
            };
            no_metadata(&fresh);
            no_metadata(&support.artifacts);
            let joined = support.merge(&support).unwrap();
            no_metadata(&support.artifacts);
            no_metadata(&joined.artifacts);
            assert_eq!(
                joined.input_decisions.recorded_facts(),
                support.input_decisions.recorded_facts()
            );
            assert!(joined
                .native_availability
                .same_recorded_facts(&support.native_availability));
            // Explicit complete observation remains available, and agrees
            // with the original independently issued graph and payload set.
            let actual = joined.artifacts.metadata_snapshot();
            let expected = issued.artifacts.metadata_snapshot();
            assert_eq!(actual.artifacts, expected.artifacts);
            assert_eq!(actual.entries, expected.entries);
            assert_eq!(actual.dependencies, expected.dependencies);
            assert_eq!(
                actual.selected_native_groups,
                expected.selected_native_groups
            );
            assert_eq!(
                actual.ambiguous_native_owners,
                expected.ambiguous_native_owners
            );
        }
    }

    #[test]
    fn canonical_id_without_entry_matches_independent_descriptor_encoding() {
        use sha2::Digest;
        for producer in [[2; 32], [7; 32]] {
            for module in ["CanonicalA", "CanonicalB"] {
                let interface = crate::certified_products::fixture_module_interface(
                    producer,
                    "fixture",
                    module,
                    BTreeMap::from([(("fixture".into(), "Dependency".into()), [9; 32])]),
                );
                let mut expected = ArtifactDescriptor {
                    id: ArtifactId([0; 32]),
                    kind: ArtifactKind::CanonicalModuleInterface,
                    owner: ExactModuleIdentity {
                        unit: "fixture".into(),
                        module: module.into(),
                    },
                    producer_sha256: producer,
                    interface_sha256: interface.interface_sha256(),
                    product_sha256: None,
                    package_imports_sha256: interface.package_imports_sha256(),
                    certification_sha256: Some(
                        sha2::Sha256::digest(interface.certificate_bytes()).into(),
                    ),
                };
                expected.id =
                    ArtifactId(sha2::Sha256::digest(serde_json::to_vec(&expected).unwrap()).into());
                assert_eq!(
                    ArtifactEntry::canonical_artifact_id(&interface),
                    expected.id
                );
                let entry = ArtifactEntry::canonical(interface);
                assert_eq!(entry.descriptor, expected);
                assert_eq!(entry.requirements.len(), 1);
            }
        }
    }

    #[test]
    fn selected_native_acquisitions_follow_the_whole_winning_row() {
        let entries = decision_affinity_entries();
        let inventory = ArtifactInventory::default();
        let view = inventory
            .admit_shared(&inventory.empty_view(), entries[..2].to_vec())
            .unwrap();
        let projection = CompilerInputProjection::from_issued_entries(&entries[..2]).unwrap();
        let a = CompilerSourceSupport::from_selected_issuance(projection.clone(), view.clone())
            .unwrap();
        let b = CompilerSourceSupport::from_selected_issuance(projection, view).unwrap();
        assert!(a.same_recorded_facts(&b));
        let owner = &entries[1].descriptor.owner;
        let group = a.input_decisions.facts[owner].groups[&7];
        let selected = |source: &CompilerSourceSupport| {
            source
                .compiler_roles()
                .selected_native_support(owner, group)
                .unwrap()
                .acquisitions()[0]
                .clone()
        };
        let issuer_a = selected(&a);
        let issuer_b = selected(&b);
        assert!(!issuer_a.same_issuance(&issuer_b));
        for (receiving, incoming) in [(&a, &b), (&b, &a)] {
            let expected = selected(receiving);
            let merged = receiving.merge(incoming).unwrap();
            assert!(selected(&merged).same_issuance(&expected));
            let narrowed = merged
                .source_support(&BTreeSet::from([owner.clone()]))
                .unwrap();
            assert!(selected(&narrowed).same_issuance(&expected));
        }
        let facts = a.native_fact_owner();
        let recovered = a
            .restore_native_maps(
                &a.projection.recorded_roles,
                &facts.offers,
                &facts.native_availability,
            )
            .unwrap();
        assert!(matches!(
            recovered
                .compiler_roles()
                .selected_native_support(owner, group),
            Some(SelectedNativeSupport::Missing(_))
        ));
        let attached = recovered
            .retain_custody(&a.artifacts)
            .unwrap()
            .retain_offers(a.native_offers.iter().cloned())
            .unwrap();
        assert!(matches!(
            attached
                .compiler_roles()
                .selected_native_support(owner, group),
            Some(SelectedNativeSupport::Missing(_))
        ));
        assert!(matches!(
            recovered
                .merge(&a)
                .unwrap()
                .compiler_roles()
                .selected_native_support(owner, group),
            Some(SelectedNativeSupport::Missing(_))
        ));
        assert!(selected(&a.merge(&recovered).unwrap()).same_issuance(&issuer_a));
    }

    #[test]
    fn source_merge_keeps_native_choices_with_the_selected_exact_issuer() {
        let entries = decision_affinity_entries();
        let inventory = ArtifactInventory::default();
        let foreign_inventory = ArtifactInventory::default();
        let issue = |marker: &str, with_caller: bool, empty: bool, foreign: bool| {
            let inventory = if foreign {
                &foreign_inventory
            } else {
                &inventory
            };
            let selected = if with_caller {
                &entries[..]
            } else {
                &entries[..2]
            };
            let mut issued = selected.to_vec();
            issued.push(Arc::new(crate::artifact_inventory::tests::entry(
                marker,
                &[],
            )));
            let view = inventory
                .admit_shared_with_demand(
                    &inventory.empty_view(),
                    issued,
                    if empty {
                        NativeArtifactDemand::ScopeInterfaces
                    } else {
                        NativeArtifactDemand::AllGroups
                    },
                )
                .unwrap();
            CompilerSourceSupport::from_selected_issuance(
                CompilerInputProjection::from_issued_entries(selected).unwrap(),
                view,
            )
            .unwrap()
        };
        for (empty_winner, cross_inventory) in
            [(false, false), (true, false), (false, true), (true, true)]
        {
            let winner = issue("WinnerMarker", false, empty_winner, false);
            let historical = issue("HistoricalMarker", true, false, cross_inventory);
            let parent = &entries[1];
            let owner = &parent.descriptor.owner;
            let pair = |support: &CompilerSourceSupport| {
                let row = &support.selected[owner];
                (row.interface().0, row.original().unwrap().0)
            };
            assert_eq!(
                winner.projection.snapshot_roles()[owner],
                historical.projection.snapshot_roles()[owner]
            );
            assert_ne!(pair(&winner), pair(&historical));
            let winner_choices = winner.input_decisions.facts[owner].groups.clone();
            assert_eq!(winner_choices.is_empty(), empty_winner);
            let historical_choices = historical.input_decisions.facts[owner].groups.clone();
            assert_eq!(historical_choices.len(), 1);
            let caller = &entries[2];
            let caller_group =
                historical.input_decisions.facts[&caller.descriptor.owner].groups[&3];
            let historical_parent = historical_choices[&7];
            let expected_edge = (
                InventoryNodeKey::Group(caller_group),
                InventoryNodeKey::Group(historical_parent),
                ArtifactDependency::NativeGroup {
                    dependent_ordinal: 3,
                    required_ordinal: 7,
                },
            );
            assert!(historical
                .artifacts
                .binding_dependencies()
                .contains(&expected_edge));
            let expected_edges = winner
                .artifacts
                .binding_dependencies()
                .into_iter()
                .chain(historical.artifacts.binding_dependencies())
                .collect::<BTreeSet<_>>();
            type_query_work::take();
            let joined = winner
                .merge(&historical)
                .expect("a losing historical issuer does not replace the selected input choice");
            let merge_work = type_query_work::take();
            assert_eq!(
                merge_work.resolved_rows, 0,
                "post-solver finalization does not rediscover chosen bindings"
            );
            assert_eq!(merge_work.canonical_checks, 0);
            assert_eq!(
                merge_work.descriptor_serializations, 0,
                "joining acquired rows must not reencode their certified products"
            );
            assert_eq!(merge_work.census_rows, joined.projection.roles.len());
            assert!(!joined.selected.rows.is_empty());
            let (kernel, kernel_projection, kernel_decisions) = winner
                .artifacts
                .merge_compiler_inputs_with_demand(
                    &winner.projection,
                    &historical.artifacts,
                    &historical.projection,
                    CompilerNativeDemand::SourceSelections {
                        left: Some(&winner.input_decisions),
                        right: Some(&historical.input_decisions),
                    },
                    CompilerInputValidation::Closed,
                )
                .unwrap();
            let prior_constructor = crate::artifact_inventory::tests::reference_compiler_selection(
                &winner.artifacts,
                &historical.artifacts,
                &kernel.namespace(),
                kernel.roots(),
            );
            assert_eq!(
                kernel.capture_graph_selection(),
                prior_constructor.capture_graph_selection()
            );
            type_query_work::take();
            let mut prior_support = winner
                .rebuild(
                    kernel_projection,
                    prior_constructor,
                    winner
                        .native_offers
                        .iter()
                        .chain(historical.native_offers.iter())
                        .cloned(),
                )
                .unwrap()
                .retain_normalized_availability(historical.native_availability())
                .unwrap();
            let prior_work = type_query_work::take();
            assert!(
                prior_work.resolved_rows > 0,
                "the reference rebuild calibrates removed work"
            );
            prior_support.input_decisions = kernel_decisions;
            assert!(joined.selected.same_recorded_facts(&prior_support.selected));
            assert_eq!(
                joined.native_offer_observations().unwrap(),
                prior_support.native_offer_observations().unwrap()
            );
            for (owner, row) in joined.selected.iter() {
                let issuer = winner
                    .selected
                    .get(owner)
                    .or_else(|| historical.selected.get(owner))
                    .expect("fixture selected issuer");
                for ((binding, entry), (issued_binding, issued_entry)) in
                    row.bindings().zip(issuer.bindings())
                {
                    assert_eq!(binding, issued_binding);
                    assert!(
                        Arc::ptr_eq(entry, issued_entry),
                        "whole row retains its selected issuer"
                    );
                }
            }
            assert_eq!(
                joined.artifacts.capture_graph_selection(),
                prior_support.artifacts.capture_graph_selection(),
            );
            assert_eq!(joined.projection, prior_support.projection);
            assert_eq!(
                joined.input_decisions.recorded_facts(),
                prior_support.input_decisions.recorded_facts()
            );
            assert_eq!(
                joined.native_availability,
                prior_support.native_availability
            );
            let current_metadata = joined.artifacts.metadata_snapshot();
            let prior_metadata = prior_support.artifacts.metadata_snapshot();
            assert_eq!(current_metadata.entries, prior_metadata.entries);
            assert_eq!(current_metadata.artifacts, prior_metadata.artifacts);
            assert_eq!(
                current_metadata.selected_native_groups,
                prior_metadata.selected_native_groups
            );
            assert_eq!(
                current_metadata.ambiguous_native_owners,
                prior_metadata.ambiguous_native_owners
            );
            drop(prior_support);
            drop(kernel);
            assert_eq!(pair(&joined), pair(&winner));
            assert_eq!(joined.input_decisions.facts[owner].groups, winner_choices);
            let selected_groups = joined.artifacts.capture_graph_selection().native_groups;
            assert!(selected_groups.contains(&caller_group));
            assert!(
                selected_groups.contains(&historical_parent),
                "the losing parent's exact Group remains selected through its actual caller root"
            );
            assert!(expected_edges.is_subset(
                &joined
                    .artifacts
                    .binding_dependencies()
                    .into_iter()
                    .collect()
            ));
            let reverse = historical.merge(&winner).unwrap();
            assert_eq!(pair(&reverse), pair(&historical));
            assert_eq!(
                reverse.input_decisions.facts[owner].groups,
                historical_choices
            );
            drop(reverse);
            drop(winner);
            drop(historical);
            assert!(joined
                .artifacts
                .binding_dependencies()
                .contains(&expected_edge));
        }
    }

    #[test]
    fn source_choice_affinity_does_not_resolve_unselected_ambiguous_custody() {
        let entries = decision_affinity_entries();
        let inventory = ArtifactInventory::default();
        let issue = |marker: &str, with_caller: bool| {
            let selected = if with_caller {
                &entries[..]
            } else {
                &entries[..2]
            };
            let mut issued = selected.to_vec();
            issued.push(Arc::new(crate::artifact_inventory::tests::entry(
                marker,
                &[],
            )));
            let view = inventory
                .admit_shared(&inventory.empty_view(), issued)
                .unwrap();
            CompilerSourceSupport::from_selected_issuance(
                CompilerInputProjection::from_issued_entries(selected).unwrap(),
                view,
            )
            .unwrap()
        };
        let winner = issue("SelectedNamespaceMarker", false);
        let historical = issue("HistoricalNamespaceMarker", true);
        let extra = issue("CustodyOnlyMarker", false);
        let mut losing = historical
            .artifacts
            .retain_custody(&extra.artifacts)
            .unwrap();
        // Exercise the artifact merge boundary with authentic issued choices
        // and graphs, but no losing namespace endorsement. Metadata role
        // equality cannot resolve or promote this ambiguous byte custody.
        losing.replace_namespace(&BTreeMap::new());
        let parent = &entries[1];
        {
            let state = losing.lease.inventory.0.lock().unwrap();
            let owned = &losing.read_projection(&state).nodes;
            assert!(
                matches!(losing.resolve_binding(parent.descriptor.id, owned),
                Err(CompileError::ArtifactInventory(error))
                    if matches!(error.failure, ArtifactInventoryFailure::BindingAmbiguity { artifact }
                        if artifact == parent.descriptor.id))
            );
        }
        let expected = losing
            .binding_dependencies()
            .into_iter()
            .collect::<BTreeSet<_>>();
        let (joined, projection, decisions) = winner
            .artifacts
            .merge_compiler_inputs_with_demand(
                &winner.projection,
                &losing,
                &historical.projection,
                CompilerNativeDemand::SourceSelections {
                    left: Some(&winner.input_decisions),
                    right: Some(&historical.input_decisions),
                },
                CompilerInputValidation::Closed,
            )
            .expect("unselected ambiguous custody cannot veto an exact selected choice");
        assert_eq!(
            projection,
            winner.projection.merge(&historical.projection).unwrap()
        );
        assert_eq!(
            decisions.facts[&parent.descriptor.owner].groups,
            winner.input_decisions.facts[&parent.descriptor.owner].groups
        );
        let selected = joined.capture_graph_selection().native_groups;
        let caller_group =
            historical.input_decisions.facts[&entries[2].descriptor.owner].groups[&3];
        let historical_parent =
            historical.input_decisions.facts[&parent.descriptor.owner].groups[&7];
        assert!(selected.contains(&caller_group));
        assert!(selected.contains(&historical_parent));
        drop(extra);
        drop(historical);
        drop(losing);
        assert!(expected.is_subset(&joined.binding_dependencies().into_iter().collect()));
    }

    #[test]
    fn source_merge_refuses_different_group_choices_with_the_same_exact_issuer() {
        let entries = decision_affinity_entries();
        let inventory = ArtifactInventory::default();
        let base = inventory
            .admit_shared_with_demand(
                &inventory.empty_view(),
                entries[..2].to_vec(),
                NativeArtifactDemand::ScopeInterfaces,
            )
            .unwrap();
        let groups = BTreeSet::from([
            NativeGroupKey {
                artifact: entries[0].descriptor.id,
                original_ordinal: 9,
            },
            NativeGroupKey {
                artifact: entries[1].descriptor.id,
                original_ordinal: 7,
            },
        ]);
        let supports = ["FirstGroupMarker", "SecondGroupMarker"].map(|marker| {
            let mut issued = entries[..2].to_vec();
            issued.push(Arc::new(crate::artifact_inventory::tests::entry(
                marker,
                &[],
            )));
            let view = inventory
                .admit_recovery_selection(&base, issued, &groups)
                .unwrap();
            CompilerSourceSupport::from_selected_issuance(
                CompilerInputProjection::from_issued_entries(&entries[..2]).unwrap(),
                view,
            )
            .unwrap()
        });
        for entry in &entries[..2] {
            let owner = &entry.descriptor.owner;
            assert!(supports[0].selected[owner].same_recorded_facts(&supports[1].selected[owner]));
            assert_ne!(
                supports[0].input_decisions.facts[owner].groups,
                supports[1].input_decisions.facts[owner].groups
            );
        }
        for (left, right) in [(&supports[0], &supports[1]), (&supports[1], &supports[0])] {
            assert!(
                matches!(left.merge(right), Err(CompileError::ArtifactInventory(error))
                if matches!(error.failure, ArtifactInventoryFailure::BindingAmbiguity { artifact }
                    if entries[..2].iter().any(|entry| entry.descriptor.id == artifact)))
            );
        }
    }

    fn decision_affinity_entries() -> Vec<Arc<ArtifactEntry>> {
        use crate::certified_products::tests::{
            original_groups_fixture_with_interface, recovered_witness_fixtures,
        };
        use crate::certified_products::{fixture_finalized_product, PendingImportOwner};
        let product = |module: &str, ordinal, imports| {
            fixture_finalized_product(
                original_groups_fixture_with_interface(
                    module,
                    vec![(ordinal, imports)],
                    1,
                    &BTreeMap::new(),
                    module.as_bytes().to_vec(),
                ),
                [2; 32],
            )
        };
        let import = |product: &crate::recovery_artifacts::CertifiedRecoveryProduct, ordinal| {
            let definitions =
                crate::certified_products::original_native_group_definitions(product, ordinal)
                    .unwrap();
            let binder = match &definitions.bindings()[0] {
                tidepool_repr::execution_schema::Group::NonRecursive(binding) => &binding.identity,
                tidepool_repr::execution_schema::Group::Recursive(bindings) => {
                    &bindings[0].identity
                }
            }
            .clone();
            PendingImportOwner::Source {
                owner: product.owner().clone(),
                original_ordinal: ordinal,
                binder,
            }
        };
        let child = product("AffinityChild", 9, vec![]);
        let parent = product("AffinityParent", 7, vec![import(&child, 9)]);
        let caller = product("AffinityCaller", 3, vec![import(&parent, 7)]);
        recovered_witness_fixtures(&[child, parent, caller])
            .into_iter()
            .map(|row| Arc::new(ArtifactEntry::original([2; 32], row.product).unwrap()))
            .collect()
    }

    #[test]
    fn custody_only_support_additions_preserve_qualified_rows_and_selected_owners() {
        let product = crate::certified_products::fixture_finalized_product(
            crate::certified_products::tests::original_groups_fixture(
                "CustodyPermissions",
                vec![(7, vec![]), (9001, vec![])],
                1,
                &BTreeMap::new(),
            ),
            [2; 32],
        );
        let product = crate::certified_products::tests::recovered_witness_fixtures(&[product])
            .remove(0)
            .product;
        let entry = Arc::new(ArtifactEntry::original([2; 32], product).unwrap());
        let projection = CompilerInputProjection::from_issued_entries(&[entry.clone()]).unwrap();
        for foreign in [false, true] {
            let inventory = ArtifactInventory::default();
            let incoming_inventory = if foreign {
                ArtifactInventory::default()
            } else {
                inventory.clone()
            };
            let full = incoming_inventory
                .admit_shared(&incoming_inventory.empty_view(), vec![entry.clone()])
                .unwrap();
            let incoming =
                CompilerSourceSupport::from_selected_issuance(projection.clone(), full).unwrap();
            for ordinals in [Some(vec![7, 9001]), Some(vec![7]), Some(vec![]), None] {
                let selected = ordinals
                    .as_deref()
                    .unwrap_or(&[])
                    .iter()
                    .map(|ordinal| NativeGroupKey {
                        artifact: entry.descriptor.id,
                        original_ordinal: *ordinal,
                    })
                    .collect();
                let view = inventory
                    .admit_recovery_selection(
                        &inventory.empty_view(),
                        vec![entry.clone()],
                        &selected,
                    )
                    .unwrap();
                let support = if ordinals.is_some() {
                    CompilerSourceSupport::from_selected_issuance(projection.clone(), view).unwrap()
                } else {
                    CompilerSourceSupport::new(
                        projection.for_source_owners(&BTreeSet::new()),
                        view,
                        [],
                        Default::default(),
                    )
                    .unwrap()
                };
                let rows = support
                    .native_availability
                    .rows()
                    .cloned()
                    .collect::<Vec<_>>();
                assert_eq!(rows.len(), usize::from(ordinals.is_some()));
                for row in &rows {
                    assert_eq!(row.permits(7), ordinals.as_ref().unwrap().contains(&7));
                    assert_eq!(
                        row.permits(9001),
                        ordinals.as_ref().unwrap().contains(&9001)
                    );
                }
                let original_namespace = support.artifacts.namespace();
                let original_groups = support.artifacts.selected_native_groups();
                let original_decisions = support.input_decisions.clone();
                let expected_edges = incoming
                    .artifacts
                    .binding_dependencies()
                    .into_iter()
                    .filter(|(from, to, _)| {
                        matches!(
                            (from, to),
                            (
                                ArtifactBindingNode::Artifact(_),
                                ArtifactBindingNode::Artifact(_)
                            )
                        )
                    })
                    .collect::<Vec<_>>();
                assert!(
                    !expected_edges.is_empty(),
                    "incoming native carriers have actual interface edges"
                );
                let custody_oracle = support
                    .rebuild(
                        support.projection.clone(),
                        support
                            .artifacts
                            .retain_custody(&incoming.artifacts)
                            .unwrap(),
                        support.native_offers.iter().cloned(),
                    )
                    .unwrap();
                let offers_oracle = support
                    .rebuild(
                        support.projection.clone(),
                        support.artifacts.clone(),
                        support
                            .native_offers
                            .iter()
                            .chain(incoming.native_offers.iter())
                            .cloned(),
                    )
                    .unwrap();
                type_query_work::take();
                let custody = support.retain_custody(&incoming.artifacts).unwrap();
                let offers = support
                    .retain_offers(incoming.native_offers.iter().cloned())
                    .unwrap();
                let work = type_query_work::take();
                assert_eq!(
                    work.resolved_rows, 0,
                    "custody does not rederive selected associations"
                );
                assert_eq!(
                    work.canonical_checks, 0,
                    "custody does not revalidate selected interfaces"
                );
                assert_eq!(custody, custody_oracle);
                assert_eq!(offers.len(), offers_oracle.len());
                assert!(offers
                    .iter()
                    .zip(&offers_oracle)
                    .all(|(actual, expected)| actual.same_recorded_facts(expected)));
                drop(custody_oracle);
                drop(offers_oracle);
                for retained in [custody, offers] {
                    assert_eq!(retained.artifacts.namespace(), original_namespace);
                    assert_eq!(retained.artifacts.selected_native_groups(), original_groups);
                    assert_eq!(
                        retained.input_decisions.recorded_facts(),
                        original_decisions.recorded_facts()
                    );
                    assert_eq!(retained.native_availability.rows().count(), rows.len());
                    for (row, prior) in retained.native_availability.rows().zip(&rows) {
                        assert!(Arc::ptr_eq(row, prior));
                    }
                    assert!(
                        Arc::ptr_eq(&retained.selected, &support.selected),
                        "custody growth cannot replace the selected row owner"
                    );
                    assert!(Arc::ptr_eq(
                        &retained.selected.home_units,
                        &support.selected.home_units
                    ));
                    if retained.native_offers.len() > support.native_offers.len() {
                        assert!(retained.native_offers.iter().any(|offer| offer.is_full()));
                    }
                    // The exact graph is retained as custody, not borrowed from
                    // the incoming support's selected namespace or demand.
                    let edges = retained.artifacts.binding_dependencies();
                    assert!(expected_edges.iter().all(|edge| edges.contains(edge)));
                }
            }
        }
    }

    #[test]
    fn namespace_finalization_reuses_only_an_unchanged_exact_view() {
        let entries = decision_affinity_entries();
        let inventory = ArtifactInventory::default();
        let projection = CompilerInputProjection::from_issued_entries(&entries).unwrap();
        for empty in [false, true] {
            let mut issued = entries.clone();
            issued.push(Arc::new(crate::artifact_inventory::tests::entry(
                "OutsideCompilerNamespace",
                &[],
            )));
            let view = inventory
                .admit_shared_with_demand(
                    &inventory.empty_view(),
                    issued,
                    if empty {
                        NativeArtifactDemand::ScopeInterfaces
                    } else {
                        NativeArtifactDemand::AllGroups
                    },
                )
                .unwrap();
            let support =
                CompilerSourceSupport::from_selected_issuance(projection.clone(), view).unwrap();
            assert_eq!(support.input_decisions.facts.len(), entries.len());
            for (entry, ordinal) in entries.iter().zip([9, 7, 3]) {
                let choice = &support.input_decisions.facts[&entry.descriptor.owner];
                let expected = if empty { vec![] } else { vec![ordinal] };
                assert_eq!(choice.groups.keys().copied().collect::<Vec<_>>(), expected);
            }
            let parent = entries[1].descriptor.id;
            assert_eq!(
                support
                    .native_offers
                    .iter()
                    .find(|offer| offer.original() == parent)
                    .expect("the issued parent has an actual native offer")
                    .is_full(),
                !empty,
            );
            assert!(matches!(
                support
                    .artifacts
                    .with_compiler_projection(&support.projection)
                    .unwrap(),
                std::borrow::Cow::Owned(_)
            ));
            let prior_finalize = |support: &CompilerSourceSupport| {
                support
                    .rebuild(
                        support.projection.clone(),
                        support
                            .artifacts
                            .with_compiler_projection(&support.projection)
                            .unwrap()
                            .into_owned(),
                        support.native_offers.iter().cloned(),
                    )
                    .unwrap()
            };
            let expected = prior_finalize(&support);
            let finalized = support.finalize_namespace().unwrap();
            assert!(finalized.same_recorded_facts(&expected));
            assert_eq!(
                finalized.artifacts.capture_graph_selection(),
                expected.artifacts.capture_graph_selection()
            );
            assert!(!Arc::ptr_eq(&support.selected, &finalized.selected));
            let expected = prior_finalize(&finalized);
            for _ in 0..2 {
                let _ = type_query_work::take();
                let repeated = finalized.finalize_namespace().unwrap();
                let work = type_query_work::take();
                assert_eq!(work.resolved_rows, 0);
                assert_eq!(work.canonical_checks, 0);
                assert!(Arc::ptr_eq(&repeated.selected, &finalized.selected));
                assert!(repeated.same_recorded_facts(&expected));
                assert_eq!(
                    repeated.artifacts.namespace(),
                    finalized.artifacts.namespace()
                );
                assert_eq!(
                    repeated.artifacts.capture_graph_selection(),
                    expected.artifacts.capture_graph_selection()
                );
            }
            let type_only = finalized
                .rebuild(
                    finalized.projection.interface_only(),
                    finalized.artifacts.clone(),
                    finalized.native_offers.iter().cloned(),
                )
                .unwrap();
            assert!(matches!(
                type_only
                    .artifacts
                    .with_compiler_projection(&type_only.projection)
                    .unwrap(),
                std::borrow::Cow::Owned(_)
            ));
            let expected = prior_finalize(&type_only);
            let narrowed = type_only.finalize_namespace().unwrap();
            assert!(!Arc::ptr_eq(&type_only.selected, &narrowed.selected));
            if !empty {
                use crate::declaration_context::NativeTransportMode;
                let before = type_only.native_availability.snapshot_facts().unwrap();
                let after = narrowed.native_availability.snapshot_facts().unwrap();
                assert_eq!(before[&parent].mode, NativeTransportMode::Full);
                assert_eq!(after[&parent].mode, NativeTransportMode::Full);
            }
            assert!(narrowed.same_recorded_facts(&expected));
            assert_eq!(
                narrowed.native_availability.snapshot_facts().unwrap(),
                expected.native_availability.snapshot_facts().unwrap()
            );
            assert!(entries.iter().all(|entry| {
                !narrowed
                    .artifacts
                    .namespace()
                    .contains_key(&entry.descriptor.id)
            }));
            assert_eq!(
                narrowed.input_decisions.recorded_facts(),
                expected.input_decisions.recorded_facts()
            );
        }
    }

    #[test]
    fn namespace_projection_preserves_exact_issuer_and_refuses_ambiguous_bindings() {
        let entries = decision_affinity_entries();
        let inventory = ArtifactInventory::default();
        let projection = CompilerInputProjection::from_issued_entries(&entries).unwrap();
        let [left, right] = ["NamespaceLeft", "NamespaceRight"].map(|marker| {
            let mut issued = entries.clone();
            issued.push(Arc::new(crate::artifact_inventory::tests::entry(
                marker,
                &[],
            )));
            let view = inventory
                .admit_shared(&inventory.empty_view(), issued)
                .unwrap();
            view.with_compiler_projection(&projection)
                .unwrap()
                .into_owned()
        });
        for entry in &entries {
            assert_eq!(
                left.lease.payloads[&entry.descriptor.id],
                right.lease.payloads[&entry.descriptor.id],
            );
        }
        assert_ne!(
            left.namespace()[&entries[1].descriptor.id],
            right.namespace()[&entries[1].descriptor.id]
        );
        for view in [&left, &right] {
            let projected = view.with_compiler_projection(&projection).unwrap();
            assert!(matches!(&projected, std::borrow::Cow::Borrowed(_)));
            assert_eq!(projected.namespace(), view.namespace());
        }
        let mut custody = left.retain_custody(&right).unwrap();
        custody.replace_namespace(&BTreeMap::new());
        assert!(matches!(
            custody.with_compiler_projection(&projection),
            Err(CompileError::ArtifactInventory(error))
                if matches!(error.failure, ArtifactInventoryFailure::BindingAmbiguity { .. })
        ));
        assert!(matches!(
            custody
                .with_compiler_projection(&CompilerInputProjection::default())
                .unwrap(),
            std::borrow::Cow::Borrowed(_)
        ));
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

#[cfg(test)]
#[path = "compiler_projection_acquisition_tests.rs"]
mod acquisition_tests;
