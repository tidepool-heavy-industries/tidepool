//! Compiler input roles are issued independently of retained native custody.
//! Serialized roles become authority only after checking their exact originals.

use super::*;

/// Historical native authority is issued with a publication. Transport records
/// cannot be consumed until bound to the receiving context's acquired view.
#[derive(Clone, Debug)]
pub struct PublicationNativeAuthority(PublicationNativeState);
#[derive(Clone, Debug)]
enum PublicationNativeState {
    Recorded(Arc<crate::exact_context::PublicationNativeFacts>),
    Acquired(Arc<PublicationNativeInputs>),
}
#[derive(Debug)]
struct PublicationNativeInputs {
    facts: Arc<crate::exact_context::PublicationNativeFacts>,
    offers: Arc<[IssuedNativeOffer]>,
    availability: crate::declaration_context::RequestNativeAvailability,
    completions: NativeSelectionCompletions,
    decisions: IssuedInputNativeDecisions,
    artifacts: ArtifactView,
    projection: CompilerInputProjection,
    selected: Arc<AdmittedCompilerRoles>,
}
impl PublicationNativeAuthority {
    pub(crate) fn facts(&self) -> &Arc<crate::exact_context::PublicationNativeFacts> {
        match &self.0 {
            PublicationNativeState::Recorded(facts) => facts,
            PublicationNativeState::Acquired(inputs) => &inputs.facts,
        }
    }
    pub(crate) fn recorded(facts: Arc<crate::exact_context::PublicationNativeFacts>) -> Self {
        Self(PublicationNativeState::Recorded(facts))
    }
    pub(crate) fn capture(source: &CompilerSourceSupport) -> Result<Self, CompileError> {
        Ok(Self(PublicationNativeState::Acquired(Arc::new(
            PublicationNativeInputs {
                facts: Arc::clone(source.native_fact_owner()),
                offers: Arc::clone(&source.native_offers),
                availability: source.native_availability.clone(),
                completions: source.native_completions.clone(),
                decisions: source.input_decisions.clone(),
                artifacts: source.artifacts.clone(),
                projection: source.projection.clone(),
                selected: Arc::clone(&source.selected),
            },
        ))))
    }
    fn bind(
        &self,
        artifacts: &ArtifactView,
        projection: &CompilerInputProjection,
        owner: &ExactModuleIdentity,
    ) -> Result<Self, CompileError> {
        let role = projection
            .roles
            .get(owner)
            .ok_or_else(|| failure("publication native authority lacks its role"))?;
        let CompilerInputRole::PublishedSourceOriginal {
            original,
            interface,
            selection,
            ..
        } = role
        else {
            return Err(failure(
                "publication native authority lacks published selection",
            ));
        };
        let facts = Arc::clone(self.facts());
        if facts.roles.get(owner).is_none_or(|role| {
            role != &CompilerInputRole::ReusableOriginal {
                interface: *interface,
                original: *original,
            }
        }) {
            return Err(failure(
                "publication birth root is not its exact pre-endorsement role",
            ));
        }
        let artifacts = artifacts.select_issued(vec![*original], selection)?;
        let prior = CompilerInputProjection::acquire_recorded(&artifacts, &facts.roles)?
            .bind_publications(&artifacts)?;
        let empty = CompilerSourceSupport::new(prior.clone(), artifacts, [], Default::default())?;
        let mut restored = empty.restore_native_maps(
            &prior.recorded_roles,
            &facts.offers,
            &facts.native_availability,
            &facts.input_decisions,
        )?;
        if let PublicationNativeState::Acquired(inputs) = &self.0 {
            let mut draft = restored.reopen();
            draft.append_completion_refs(&inputs.completions)?;
            draft.input_decisions = inputs
                .decisions
                .with_recorded_facts(draft.input_decisions.facts.clone());
            restored = draft.close()?;
        }
        Ok(Self(PublicationNativeState::Acquired(Arc::new(
            PublicationNativeInputs {
                facts,
                offers: Arc::clone(&restored.native_offers),
                availability: restored.native_availability,
                completions: restored.native_completions,
                decisions: restored.input_decisions,
                artifacts: restored.artifacts,
                projection: prior,
                selected: restored.selected,
            },
        ))))
    }
    fn acquired(&self) -> Result<&PublicationNativeInputs, CompileError> {
        match &self.0 {
            PublicationNativeState::Acquired(inputs) => Ok(inputs),
            _ => Err(failure(
                "publication native authority has not acquired its receiving view",
            )),
        }
    }
}
impl PartialEq for PublicationNativeAuthority {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(self.facts(), other.facts()) || self.facts() == other.facts()
    }
}
impl Eq for PublicationNativeAuthority {}
impl Serialize for PublicationNativeAuthority {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.facts().serialize(serializer)
    }
}
impl<'de> Deserialize<'de> for PublicationNativeAuthority {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::recorded(Arc::new(
            crate::exact_context::PublicationNativeFacts::deserialize(deserializer)?,
        )))
    }
}

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
        native_authority: PublicationNativeAuthority,
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
    /// A recorded role still needs the existing receiving-view acquisition.
    pub(crate) fn to_recorded_role(&self) -> CompilerInputRole {
        match self {
            Self::InterfaceOnly { interface } => CompilerInputRole::InterfaceOnly {
                interface: *interface,
            },
            Self::ReusableOriginal {
                interface,
                original,
            } => CompilerInputRole::ReusableOriginal {
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
            } => CompilerInputRole::PublishedSourceOriginal {
                interface: *interface,
                original: *original,
                source_revision: source_revision.clone(),
                original_input_identity: original_input_identity.clone(),
                selection: selection.clone(),
                selection_sha256: *selection_sha256,
                native_authority: PublicationNativeAuthority::recorded(Arc::clone(
                    native_authority,
                )),
            },
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
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CompilerInputProjection {
    roles: imbl::OrdMap<ExactModuleIdentity, CompilerInputRole>,
    // Updated only with changed roles. Snapshots borrow this pure fact root;
    // live roles separately retain their actual acquired publication owners.
    recorded_roles: imbl::OrdMap<ExactModuleIdentity, RecordedCompilerRole>,
}

/// Closed immutable destinations selected for one HOME global slot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CompletedNativeImport {
    Source {
        binding: NativeGroupBinding,
        value: tidepool_repr::execution_schema::ValueId,
    },
    Retained {
        binding: ArtifactBinding,
        identity: tidepool_repr::execution_schema::SymbolIdentity,
        generation: u64,
    },
    Package {
        binding: super::PackageCodeBinding,
        value: tidepool_repr::execution_schema::ValueId,
    },
}

#[derive(Clone, Debug)]
pub struct CompletedNativeSourceRow {
    binding: NativeGroupBinding,
    product: Arc<crate::certified_products::PendingCertifiedGroup>,
    imports: Arc<[CompletedNativeImport]>,
}
impl CompletedNativeSourceRow {
    pub fn binding(&self) -> NativeGroupBinding {
        self.binding
    }
    pub fn product(&self) -> &Arc<crate::certified_products::PendingCertifiedGroup> {
        &self.product
    }
    pub fn imports(&self) -> &[CompletedNativeImport] {
        &self.imports
    }
}
impl PartialEq for CompletedNativeSourceRow {
    fn eq(&self, other: &Self) -> bool {
        self.binding == other.binding && self.imports == other.imports
    }
}
impl Eq for CompletedNativeSourceRow {}

/// Immutable scalar completion facts are authenticated against acquired code
/// and exact scoped edges before they can restore a live completion owner.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NativeCompletionSnapshotFacts {
    pub(crate) version: u32,
    pub(crate) producer_sha256: [u8; 32],
    pub(crate) profile_sha256: [u8; 32],
    #[serde(with = "native_completion_rows_wire")]
    pub(crate) rows: imbl::OrdMap<NativeGroupBinding, Arc<NativeCompletionRowSnapshotFacts>>,
    pub(crate) package_roots: Arc<[ArtifactBinding]>,
}
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NativeCompletionRowSnapshotFacts {
    pub(crate) imports: Arc<[NativeCompletionImportSnapshotFacts]>,
}
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) enum NativeCompletionImportSnapshotFacts {
    Source {
        binding: NativeGroupBinding,
        #[serde(with = "native_completion_value_wire")]
        value: tidepool_repr::execution_schema::ValueId,
    },
    Retained {
        binding: ArtifactBinding,
        #[serde(with = "native_completion_symbol_wire")]
        identity: tidepool_repr::execution_schema::SymbolIdentity,
        generation: u64,
    },
    Package {
        binding: ArtifactBinding,
        #[serde(with = "native_completion_value_wire")]
        value: tidepool_repr::execution_schema::ValueId,
    },
}
mod native_completion_value_wire {
    use tidepool_repr::execution_schema::ValueId;
    pub(super) fn serialize<S: serde::Serializer>(
        value: &ValueId,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.serialize_u32(value.0)
    }
    pub(super) fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<ValueId, D::Error> {
        use serde::Deserialize;
        u32::deserialize(deserializer).map(ValueId)
    }
}
mod native_completion_symbol_wire {
    use tidepool_repr::execution_schema::SymbolIdentity;
    pub(super) fn serialize<S: serde::Serializer>(
        value: &SymbolIdentity,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        use serde::Serialize;
        (
            &value.unit,
            &value.module,
            &value.namespace,
            &value.occurrence,
            &value.record_parent,
        )
            .serialize(serializer)
    }
    pub(super) fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<SymbolIdentity, D::Error> {
        use serde::Deserialize;
        let (unit, module, namespace, occurrence, record_parent) =
            <(String, String, String, String, Option<String>)>::deserialize(deserializer)?;
        Ok(SymbolIdentity {
            unit,
            module,
            namespace,
            occurrence,
            record_parent,
        })
    }
}
mod native_completion_rows_wire {
    use super::*;
    pub(super) fn serialize<S: serde::Serializer>(
        rows: &imbl::OrdMap<NativeGroupBinding, Arc<NativeCompletionRowSnapshotFacts>>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq;
        let mut sequence = serializer.serialize_seq(Some(rows.len()))?;
        for row in rows {
            sequence.serialize_element(&row)?;
        }
        sequence.end()
    }
    pub(super) fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<imbl::OrdMap<NativeGroupBinding, Arc<NativeCompletionRowSnapshotFacts>>, D::Error>
    {
        use serde::Deserialize;
        let rows = Vec::<(NativeGroupBinding, Arc<NativeCompletionRowSnapshotFacts>)>::deserialize(
            deserializer,
        )?;
        if rows.windows(2).any(|pair| pair[0].0 >= pair[1].0) {
            return Err(serde::de::Error::custom(
                "noncanonical native completion row keys",
            ));
        }
        Ok(rows.into_iter().collect())
    }
}
impl NativeCompletionSnapshotFacts {
    pub(crate) fn identity(&self) -> Result<[u8; 32], CompileError> {
        self.identity_in(None)
    }
    fn identity_in(
        &self,
        operation: Option<&tidepool_repr::execution_schema::InventoryOperation>,
    ) -> Result<[u8; 32], CompileError> {
        use sha2::{Digest, Sha256};
        struct Writer<'a> {
            hash: Sha256,
            operation: Option<&'a tidepool_repr::execution_schema::InventoryOperation>,
            refused: Option<CompileError>,
        }
        impl std::io::Write for Writer<'_> {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                if let Some(operation) = self.operation {
                    if let Err(error) = operation.charge(bytes.len()) {
                        self.refused = Some(CompileError::CompilerEvidence(Box::new(error.into())));
                        return Err(std::io::Error::other("native completion commitment budget"));
                    }
                }
                self.hash.update(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut writer = Writer {
            hash: Sha256::new(),
            operation,
            refused: None,
        };
        writer
            .hash
            .update(b"Tidepool native selection completion v1\0");
        if let Err(error) = ciborium::ser::into_writer(self, &mut writer) {
            return Err(writer
                .refused
                .unwrap_or_else(|| failure(&format!("native completion identity: {error}"))));
        }
        Ok(writer.hash.finalize().into())
    }
}

/// A selected executable closure owns its immutable products and exact links.
/// It is separate from broader Original availability and Original certificate bytes.
#[derive(Debug)]
pub struct IssuedNativeSelectionCompletion {
    producer_sha256: [u8; 32],
    profile_sha256: [u8; 32],
    artifacts: ArtifactView,
    rows: imbl::OrdMap<NativeGroupBinding, Arc<CompletedNativeSourceRow>>,
    packages: Arc<super::IssuedPackageCodeGraph>,
    retained: Arc<[super::NativeBindingRequirement]>,
    recorded: Arc<NativeCompletionSnapshotFacts>,
    identity: [u8; 32],
}
impl IssuedNativeSelectionCompletion {
    fn issue_parts(
        producer_sha256: [u8; 32],
        profile_sha256: [u8; 32],
        artifacts: ArtifactView,
        rows: imbl::OrdMap<NativeGroupBinding, Arc<CompletedNativeSourceRow>>,
        packages: Arc<super::IssuedPackageCodeGraph>,
        retained: Arc<[super::NativeBindingRequirement]>,
        operation: &tidepool_repr::execution_schema::InventoryOperation,
    ) -> Result<Arc<Self>, CompileError> {
        operation
            .reserve::<NativeCompletionRowSnapshotFacts>(rows.len())
            .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
        for row in rows.values() {
            operation
                .reserve::<NativeCompletionImportSnapshotFacts>(row.imports.len())
                .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
            for import in row.imports.iter() {
                if let CompletedNativeImport::Retained { identity, .. } = import {
                    operation
                        .charge(
                            identity.unit.len()
                                + identity.module.len()
                                + identity.namespace.len()
                                + identity.occurrence.len()
                                + identity.record_parent.as_ref().map_or(0, String::len),
                        )
                        .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
                }
            }
        }
        operation
            .reserve::<ArtifactBinding>(packages.roots().len())
            .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
        let recorded = Arc::new(NativeCompletionSnapshotFacts {
            version: 1,
            producer_sha256,
            profile_sha256,
            rows: rows
                .iter()
                .map(|(binding, row)| {
                    (
                        *binding,
                        Arc::new(NativeCompletionRowSnapshotFacts {
                            imports: row
                                .imports
                                .iter()
                                .map(|link| match link {
                                    CompletedNativeImport::Source { binding, value } => {
                                        NativeCompletionImportSnapshotFacts::Source {
                                            binding: *binding,
                                            value: *value,
                                        }
                                    }
                                    CompletedNativeImport::Retained {
                                        binding,
                                        identity,
                                        generation,
                                    } => NativeCompletionImportSnapshotFacts::Retained {
                                        binding: *binding,
                                        identity: identity.clone(),
                                        generation: *generation,
                                    },
                                    CompletedNativeImport::Package { binding, value } => {
                                        NativeCompletionImportSnapshotFacts::Package {
                                            binding: binding.binding(),
                                            value: *value,
                                        }
                                    }
                                })
                                .collect::<Vec<_>>()
                                .into(),
                        }),
                    )
                })
                .collect(),
            package_roots: packages.roots().to_vec().into(),
        });
        let identity = recorded.identity_in(Some(operation))?;
        Ok(Arc::new(Self {
            producer_sha256,
            profile_sha256,
            artifacts,
            rows,
            packages,
            retained,
            recorded,
            identity,
        }))
    }
    pub(crate) fn producer_sha256(&self) -> &[u8; 32] {
        &self.producer_sha256
    }
    pub(crate) fn profile_sha256(&self) -> &[u8; 32] {
        &self.profile_sha256
    }
    pub fn rows(&self) -> impl ExactSizeIterator<Item = &Arc<CompletedNativeSourceRow>> {
        self.rows.values()
    }
    pub fn package_bindings(
        &self,
    ) -> impl ExactSizeIterator<Item = super::PackageCodeBinding> + '_ {
        self.packages.bindings()
    }
    pub fn retained_requirements(&self) -> &[super::NativeBindingRequirement] {
        &self.retained
    }
    pub(crate) fn artifacts(&self) -> &ArtifactView {
        &self.artifacts
    }
}
impl PartialEq for IssuedNativeSelectionCompletion {
    fn eq(&self, other: &Self) -> bool {
        self.producer_sha256 == other.producer_sha256
            && self.profile_sha256 == other.profile_sha256
            && self.rows == other.rows
            && self.retained == other.retained
            && self.packages.roots() == other.packages.roots()
            && self
                .packages
                .bindings()
                .map(|binding| binding.binding())
                .eq(other.packages.bindings().map(|binding| binding.binding()))
    }
}
impl Eq for IssuedNativeSelectionCompletion {}

fn restore_native_completion(
    facts: &Arc<NativeCompletionSnapshotFacts>,
    artifacts: &ArtifactView,
    validation: &mut crate::recovery_artifacts::PackageInterfaceValidation,
) -> Result<Arc<IssuedNativeSelectionCompletion>, CompileError> {
    use crate::certified_products::SelectedNativeImport;
    if facts.version != 1 || facts.producer_sha256 == [0; 32] || facts.profile_sha256 == [0; 32] {
        return Err(failure(
            "unsupported native completion facts or missing provenance",
        ));
    }
    validation
        .inventory
        .reserve::<NativeGroupBinding>(facts.rows.len())
        .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
    let bindings = facts.rows.keys().copied().collect::<BTreeSet<_>>();
    let certified = crate::certified_products::certify_acquired_original_group_rows(
        artifacts, &bindings, validation,
    )?;
    let certified = certified.rows;
    if certified.len() != bindings.len() {
        return Err(failure(
            "native completion restoration omitted acquired HOME rows",
        ));
    }
    let packages = super::IssuedPackageCodeGraph::from_issued(
        artifacts,
        &facts.package_roots,
        &validation.inventory,
    )?;
    for binding in packages.bindings() {
        let owner = binding.product().code().owner();
        if owner.compiler_sha256 != facts.producer_sha256
            || owner.profile_sha256 != facts.profile_sha256
        {
            return Err(failure(
                "restored package completion differs from issued provenance",
            ));
        }
    }
    let mut package_roots = facts.package_roots.iter();
    let mut rows = imbl::OrdMap::new();
    let mut retained = BTreeSet::new();
    for (binding, row) in &facts.rows {
        let selected = certified
            .get(binding)
            .ok_or_else(|| failure("completion HOME product is absent"))?;
        let product = selected.product();
        let acquired = artifacts
            .observed_payload(&binding.binding.artifact())
            .ok_or_else(|| failure("completion HOME payload is absent"))?;
        if acquired.descriptor.producer_sha256 != facts.producer_sha256 {
            return Err(failure(
                "completion HOME producer differs from its issued record",
            ));
        }
        let actual = selected.imports();
        let definitions = product.group().definitions();
        if row.imports.len() != actual.len() || actual.len() != definitions.globals().len() {
            return Err(failure(
                "completion HOME slot count differs from acquired body",
            ));
        }
        validation
            .inventory
            .reserve::<CompletedNativeImport>(actual.len())
            .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
        let mut imports = Vec::with_capacity(actual.len());
        for ((recorded, actual), global) in row
            .imports
            .iter()
            .zip(actual.iter())
            .zip(definitions.globals())
        {
            let import = match (recorded, actual) {
                (
                    NativeCompletionImportSnapshotFacts::Source { binding, value },
                    SelectedNativeImport::Source {
                        binding: selected,
                        value: exported,
                    },
                ) if binding == selected && value == exported && bindings.contains(selected) => {
                    CompletedNativeImport::Source {
                        binding: *binding,
                        value: *value,
                    }
                }
                (
                    NativeCompletionImportSnapshotFacts::Retained {
                        binding,
                        identity,
                        generation,
                    },
                    SelectedNativeImport::Retained {
                        binding: selected,
                        identity: observed,
                        generation: issued,
                    },
                ) if binding == selected && identity == observed && generation == issued => {
                    retained.insert(super::NativeBindingRequirement {
                        artifact_id: binding.artifact,
                        identity: identity.clone(),
                        generation: *generation,
                    });
                    CompletedNativeImport::Retained {
                        binding: *binding,
                        identity: identity.clone(),
                        generation: *generation,
                    }
                }
                (
                    NativeCompletionImportSnapshotFacts::Package { binding, value },
                    SelectedNativeImport::Package {
                        identity,
                        interface_digest,
                    },
                ) if identity == &global.identity => {
                    if package_roots.next() != Some(binding) {
                        return Err(failure(
                            "completion HOME package slot differs from its issued root order",
                        ));
                    }
                    let selected = packages.get(*binding).ok_or_else(|| {
                        failure("completion package slot is outside its acquired graph")
                    })?;
                    if selected.resolve_export(
                        global,
                        definitions.signatures(),
                        interface_digest,
                    )? != *value
                    {
                        return Err(failure(
                            "completion package export differs from its issued slot",
                        ));
                    }
                    CompletedNativeImport::Package {
                        binding: selected,
                        value: *value,
                    }
                }
                _ => {
                    return Err(failure(
                        "completion slot differs from its exact acquired input proof",
                    ))
                }
            };
            imports.push(import);
        }
        rows.insert(
            *binding,
            Arc::new(CompletedNativeSourceRow {
                binding: *binding,
                product: Arc::clone(product),
                imports: imports.into(),
            }),
        );
    }
    let completion = IssuedNativeSelectionCompletion::issue_parts(
        facts.producer_sha256,
        facts.profile_sha256,
        artifacts.retain_custody(packages.artifacts())?,
        rows,
        packages,
        retained.into_iter().collect::<Vec<_>>().into(),
        &validation.inventory,
    )?;
    if &completion.recorded != facts {
        return Err(failure(
            "native completion facts changed during acquired restoration",
        ));
    }
    Ok(completion)
}

/// The final target may require an ordinary live value. Such a requirement
/// cannot be used by the closed-source launch constructor as immutable code.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CompletedNativeTargetImport {
    Source {
        binding: NativeGroupBinding,
        value: tidepool_repr::execution_schema::ValueId,
    },
    RetainedCapture {
        identity: tidepool_repr::execution_schema::SymbolIdentity,
        generation: u64,
    },
    Package {
        binding: super::PackageCodeBinding,
        value: tidepool_repr::execution_schema::ValueId,
    },
}

/// The final target's ordered slots and its selected dependency completion are
/// coissued from the same resolved physical target. Context custody may retain
/// the dependency completion without granting another target these slots.
#[derive(Debug)]
pub struct CompletedNativeSelection {
    source_target: Arc<tidepool_repr::execution_schema::PreparedProgram>,
    source_associations: BTreeMap<NativeGroupBinding, SelectedNativeSourceAssociation>,
    completion: Arc<IssuedNativeSelectionCompletion>,
    target_imports: Arc<[CompletedNativeTargetImport]>,
    #[cfg(test)]
    package_selection: crate::certified_products::BoundPackageSelection,
}
#[derive(Debug)]
pub(crate) enum SelectedNativeSourceAssociation {
    Current(Arc<crate::certified_products::PendingCertifiedGroup>),
    Historical,
}

impl CompletedNativeSelection {
    #[cfg(test)]
    pub(crate) fn package_selection(&self) -> &crate::certified_products::BoundPackageSelection {
        &self.package_selection
    }
    pub(crate) fn source_target(&self) -> &Arc<tidepool_repr::execution_schema::PreparedProgram> {
        &self.source_target
    }
    pub(crate) fn source_association(
        &self,
        binding: NativeGroupBinding,
    ) -> Option<&SelectedNativeSourceAssociation> {
        self.source_associations.get(&binding)
    }
    pub fn completion(&self) -> &Arc<IssuedNativeSelectionCompletion> {
        &self.completion
    }
    pub fn target_imports(&self) -> &[CompletedNativeTargetImport] {
        &self.target_imports
    }
    pub fn group_imports(&self, binding: NativeGroupBinding) -> Option<&[CompletedNativeImport]> {
        self.completion
            .rows
            .get(&binding)
            .map(|row| row.imports.as_ref())
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct NativeSelectionCompletions {
    owners: imbl::OrdMap<[u8; 32], Arc<IssuedNativeSelectionCompletion>>,
    recorded: imbl::OrdMap<[u8; 32], Arc<NativeCompletionSnapshotFacts>>,
    // Shared scalar row facts index conflicts without revisiting prior owners.
    rows: imbl::OrdMap<NativeGroupBinding, Arc<NativeCompletionRowSnapshotFacts>>,
}
impl NativeSelectionCompletions {
    pub(crate) fn iter(&self) -> impl Iterator<Item = &Arc<IssuedNativeSelectionCompletion>> {
        self.owners.values()
    }
    /// Lend an already completed physical occurrence without reconstructing its
    /// product or import ownership. The caller still authenticates its selected
    /// binding closure against the current acquired scope.
    pub(crate) fn group_product(
        &self,
        binding: NativeGroupBinding,
        operation: &tidepool_repr::execution_schema::InventoryOperation,
    ) -> Result<Option<&Arc<crate::certified_products::PendingCertifiedGroup>>, CompileError> {
        let mut product: Option<&Arc<crate::certified_products::PendingCertifiedGroup>> = None;
        for completion in self.iter() {
            crate::host_work::checkpoint()?;
            operation
                .charge(1)
                .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
            if let Some(row) = completion.rows.get(&binding) {
                if product.is_some_and(|previous| !previous.same_issuance(&row.product)) {
                    return Err(failure(
                        "completed HOME binding has competing physical occurrences",
                    ));
                }
                product = Some(&row.product);
            }
        }
        Ok(product)
    }
    fn one(completion: Arc<IssuedNativeSelectionCompletion>) -> Self {
        let recorded = imbl::OrdMap::unit(completion.identity, Arc::clone(&completion.recorded));
        let rows = completion.recorded.rows.clone();
        Self {
            owners: imbl::OrdMap::unit(completion.identity, completion),
            recorded,
            rows,
        }
    }
    pub(crate) fn recorded_facts(
        &self,
    ) -> &imbl::OrdMap<[u8; 32], Arc<NativeCompletionSnapshotFacts>> {
        &self.recorded
    }
    pub(crate) fn is_empty(&self) -> bool {
        self.owners.is_empty()
    }
    pub(crate) fn append(&self, incoming: &Self) -> Result<Self, CompileError> {
        if self.owners.ptr_eq(&incoming.owners) {
            if !incoming.is_empty() {
                crate::host_work::checkpoint()?;
            }
            return Ok(self.clone());
        }
        let mut retained = self.clone();
        for completion in incoming.iter() {
            crate::host_work::checkpoint()?;
            if let Some(prior) = retained.owners.get(&completion.identity) {
                if prior.recorded != completion.recorded {
                    return Err(failure("native completion fact commitment collision"));
                }
                if !Arc::ptr_eq(prior, completion) && prior != completion {
                    return Err(failure(
                        "native completion commitment has inconsistent owners",
                    ));
                }
                continue;
            }
            retained.rows =
                append_native_completion_rows(&retained.rows, &completion.recorded.rows)?;
            retained
                .recorded
                .insert(completion.identity, Arc::clone(&completion.recorded));
            retained
                .owners
                .insert(completion.identity, Arc::clone(completion));
        }
        Ok(retained)
    }
}
impl PartialEq for NativeSelectionCompletions {
    fn eq(&self, other: &Self) -> bool {
        self.recorded == other.recorded && self.owners == other.owners
    }
}
impl Eq for NativeSelectionCompletions {}

fn append_native_completion_rows(
    retained: &imbl::OrdMap<NativeGroupBinding, Arc<NativeCompletionRowSnapshotFacts>>,
    incoming: &imbl::OrdMap<NativeGroupBinding, Arc<NativeCompletionRowSnapshotFacts>>,
) -> Result<imbl::OrdMap<NativeGroupBinding, Arc<NativeCompletionRowSnapshotFacts>>, CompileError> {
    let mut rows = retained.clone();
    for (binding, row) in incoming {
        match rows.get(binding) {
            Some(prior) if prior != row => {
                return Err(failure("native completion changed an exact HOME slot"));
            }
            Some(_) => {}
            None => {
                rows.insert(*binding, Arc::clone(row));
            }
        }
    }
    Ok(rows)
}

/// Complete only this acquired target closure. Available unused HOME groups
/// and unused captured package products do not become executable roots.
pub(crate) fn complete_native_selection(
    sources: &crate::artifacts::ResolvedNativeSourceProducts<'_>,
    output: &crate::artifacts::invocation_output::CapturedNativeOutput<'_>,
    products: &[Arc<crate::certified_products::CertifiedPackageCodeProduct>],
    selection: crate::certified_products::BoundPackageSelection,
    inherited: &NativeSelectionCompletions,
    operation: &tidepool_repr::execution_schema::InventoryOperation,
) -> Result<Arc<CompletedNativeSelection>, CompileError> {
    use crate::certified_products::{
        PackageLink, SelectedNativeImport, SelectedNativeTargetImport,
    };
    use tidepool_repr::execution_schema::DefinitionsView;

    if !std::ptr::eq(output.operation().as_ref(), operation) {
        return Err(failure(
            "native completion belongs to another output operation",
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
                "package completion differs from actual target compiler/profile",
            ));
        }
        Ok(())
    };
    let graphs = inherited
        .iter()
        .map(|old| Arc::clone(&old.packages))
        .collect::<Vec<_>>();
    // Every occurrence row names one authenticated fresh product by envelope.
    // The row's owner/defining facts must match that product exactly; the
    // envelope SHA never substitutes for selection or binding identity.
    let occurrences = selection.occurrences();
    let mut product_by_envelope = BTreeMap::new();
    for product in products {
        if product_by_envelope
            .insert(*product.envelope_sha256(), product)
            .is_some()
        {
            return Err(failure(
                "completed output contains duplicate package envelope products",
            ));
        }
    }
    let mut occurrence_products = Vec::with_capacity(occurrences.len());
    for occurrence in occurrences {
        let product = product_by_envelope
            .get(occurrence.envelope_sha256())
            .copied()
            .ok_or_else(|| {
                failure("package occurrence envelope is absent from completed output")
            })?;
        check_product(product)?;
        let owner = product.code().owner();
        if owner.unit != occurrence.unit()
            || owner.module != occurrence.module()
            || owner.interface_sha256 != *occurrence.defining_interface_sha256()
            || owner.defining_version_sha256 != *occurrence.defining_version_sha256()
        {
            return Err(failure(
                "package occurrence disagrees with its authenticated product",
            ));
        }
        if occurrence.imports().len() != product.code().globals().len() {
            return Err(failure(
                "package occurrence import count differs from its product globals",
            ));
        }
        occurrence_products.push(product);
    }
    let plan_artifact_ids = occurrence_products
        .iter()
        .map(|product| product.artifact_id())
        .collect::<Vec<_>>();
    let planned = |occurrence: u32| -> Result<super::PackageCodeTarget, CompileError> {
        let id = plan_artifact_ids
            .get(occurrence as usize)
            .copied()
            .ok_or_else(|| failure("package occurrence link is out of range"))?;
        Ok(super::PackageCodeTarget::Planned(id))
    };
    let plan_link = |link: &PackageLink| planned(link.occurrence());
    fn fresh_occurrence_matches(
        occurrence: u32,
        held: &super::PackageCodeBinding,
        occurrences: &[crate::certified_products::PackageOccurrenceRow],
        occurrence_products: &[&Arc<crate::certified_products::CertifiedPackageCodeProduct>],
        seen: &mut BTreeSet<(u32, super::ArtifactBinding)>,
        operation: &tidepool_repr::execution_schema::InventoryOperation,
    ) -> Result<bool, CompileError> {
        crate::host_work::checkpoint()?;
        operation
            .charge(1)
            .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
        let row = occurrences
            .get(occurrence as usize)
            .ok_or_else(|| failure("fresh HOME occurrence is out of range"))?;
        let product = occurrence_products
            .get(occurrence as usize)
            .ok_or_else(|| failure("fresh HOME occurrence lacks its product"))?;
        if product.as_ref() != held.product().as_ref() {
            return Ok(false);
        }
        if !seen.insert((occurrence, held.binding())) {
            return Ok(true);
        }
        let held_children = held.import_links().collect::<Vec<_>>();
        if row.imports().len() != held_children.len() {
            return Ok(false);
        }
        for (child, (held_child, _)) in row.imports().iter().zip(held_children) {
            if !fresh_occurrence_matches(
                child.occurrence(),
                &held_child,
                occurrences,
                occurrence_products,
                seen,
                operation,
            )? {
                return Ok(false);
            }
        }
        Ok(true)
    }
    enum SelectedPackageRoot {
        Occurrence(u32),
        Held(super::PackageCodeBinding),
    }
    let source_target = Arc::clone(selection.prepared_program());
    let mut source_associations = BTreeMap::new();
    let original_packages: Arc<crate::certified_products::CheckedOriginalPackageAssociations> =
        selection.checked_original_packages(operation)?;
    let selected_groups = sources
        .groups()
        .map(|row| row.binding())
        .collect::<BTreeSet<_>>();
    if !original_packages.use_roots().is_subset(&selected_groups) {
        return Err(failure(
            "Original package use exceeds the target's selected exact groups",
        ));
    }
    let mut consumed_original_slots = BTreeSet::new();
    let mut selected_package_roots = Vec::new();
    operation
        .reserve::<CompletedNativeSourceRow>(sources.groups().count())
        .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
    let mut row_inputs = Vec::new();
    for row in sources.groups() {
        let mut prior = None;
        for completed in inherited.iter() {
            if let Some(old) = completed.rows.get(&row.binding()) {
                if !old.product.same_issuance(row.group()) {
                    tracing::debug!(target: "tidepool::artifact_inventory::ambiguity_trace",
                        binding = ?row.binding(), owner = ?row.group().owner(),
                        previous_origin = ?old.product.origin(), incoming_origin = ?row.group().origin(),
                        owner_equal = old.product.owner() == row.group().owner(),
                        group_pointer_equal = std::ptr::eq(old.product.group(), row.group().group()),
                        imports_pointer_equal = std::ptr::eq(old.product.imports(), row.group().imports()),
                        physical_current = row.physical_current().is_some(),
                        "completed HOME occurrence identity refused");
                    return Err(failure("completed HOME row changed its exact product"));
                }
                if prior
                    .as_ref()
                    .is_some_and(|prior: &&Arc<CompletedNativeSourceRow>| {
                        prior.imports != old.imports
                    })
                {
                    return Err(failure("completed HOME row has competing exact slot links"));
                }
                prior = Some(old);
            }
        }
        let product = prior.map_or_else(
            || Arc::clone(row.shared_group()),
            |old| Arc::clone(&old.product),
        );
        let association = match row.source_association() {
            super::ResolvedSourceAssociation::Current => {
                SelectedNativeSourceAssociation::Current(Arc::clone(row.shared_group()))
            }
            super::ResolvedSourceAssociation::Historical => {
                SelectedNativeSourceAssociation::Historical
            }
        };
        source_associations.insert(row.binding(), association);
        let home_contract = selection
            .applicable_home_contract(row)
            .map_err(|error| CompileError::CompilerEvidence(Box::new(error)))?;
        row_inputs.push((row, prior, product, home_contract));
    }
    // Every annotation was authenticated by its issuer. Only actual selected
    // physical rows consume contracts; unused annotations never select code.
    for (row, prior, _, home_contract) in &row_inputs {
        let imports = row.imports();
        let definitions = row.group().group().definitions();
        if imports.len() != definitions.globals().len()
            || prior.is_some_and(|prior| prior.imports.len() != imports.len())
        {
            return Err(failure(
                "native completion slot count differs from its physical definitions",
            ));
        }
        let home_links = home_contract.as_ref().map(|contract| contract.links());
        let mut home_slot = 0usize;
        for (index, (link, global)) in imports.iter().zip(definitions.globals()).enumerate() {
            if let SelectedNativeImport::Package {
                identity,
                interface_digest,
            } = link
            {
                if identity != &global.identity {
                    return Err(failure(
                        "package obligation differs from its physical global",
                    ));
                }
                let exact_slot = (
                    row.binding(),
                    u32::try_from(index)
                        .map_err(|_| failure("native package slot is out of range"))?,
                );
                let original_link = original_packages
                    .slot(exact_slot.0, exact_slot.1, identity, interface_digest)
                    .map_err(|error| CompileError::CompilerEvidence(Box::new(error)))?;
                if original_link.is_some() {
                    consumed_original_slots.insert(exact_slot);
                }
                let home_link = match home_links {
                    Some(links) => {
                        let link = links.get(home_slot).ok_or_else(|| {
                            failure("selected HOME contract has too few package links")
                        })?;
                        home_slot += 1;
                        Some(link)
                    }
                    None => None,
                };
                if home_link
                    .zip(original_link)
                    .is_some_and(|(home, original)| home != original)
                {
                    return Err(failure(
                        "HOME and Original package choices conflict on an exact native slot",
                    ));
                }
                let current_link = home_link.or(original_link);
                let selected = match prior.map(|row| &row.imports[index]) {
                    Some(CompletedNativeImport::Package { binding, .. }) => {
                        check_product(binding.product())?;
                        if let Some(slot) = current_link {
                            if !fresh_occurrence_matches(
                                slot.occurrence(),
                                binding,
                                occurrences,
                                &occurrence_products,
                                &mut BTreeSet::new(),
                                operation,
                            )? {
                                return Err(failure(
                                    "fresh HOME evidence disagrees with its prior exact binding",
                                ));
                            }
                        }
                        SelectedPackageRoot::Held(binding.clone())
                    }
                    Some(_) => return Err(failure("completed HOME slot changed its import kind")),
                    None => {
                        let slot = current_link.ok_or_else(|| {
                            failure(&format!(
                                "HOME package slot lacks either prior completion or current \
                                     links: owner={:?} ordinal={} binding={:?} package={:?} slot={index}",
                                row.group().owner(),
                                row.group().group().original_ordinal(),
                                row.binding(),
                                identity,
                            ))
                        })?;
                        SelectedPackageRoot::Occurrence(slot.occurrence())
                    }
                };
                selected_package_roots.push(selected);
            }
        }
        if let Some(links) = home_links {
            if home_slot != links.len() {
                return Err(failure(
                    "selected HOME package links were not fully consumed",
                ));
            }
        }
    }
    if original_packages.coordinates().collect::<BTreeSet<_>>() != consumed_original_slots {
        return Err(failure(
            "Original package associations exceed the selected exact native slots",
        ));
    }
    let target = sources.target();
    let definitions = target.definitions();
    if sources.target_requirements().len() != definitions.globals().len() {
        return Err(failure(
            "target completion slots differ from physical definitions",
        ));
    }
    let target_links = selection.target_roots();
    let target_package_count = sources
        .target_requirements()
        .iter()
        .filter(|link| matches!(link, SelectedNativeTargetImport::Package { .. }))
        .count();
    if target_package_count != target_links.len() {
        return Err(failure(
            "target package link count differs from its physical globals",
        ));
    }
    let mut target_cursor = 0usize;
    for (link, global) in sources
        .target_requirements()
        .iter()
        .zip(definitions.globals())
    {
        match link {
            SelectedNativeTargetImport::Package {
                identity,
                interface_digest: _,
            } => {
                if identity != &global.identity {
                    return Err(failure(
                        "target package obligation differs from physical global",
                    ));
                }
                let slot = target_links
                    .get(target_cursor)
                    .ok_or_else(|| failure("target package slot exceeds package links"))?;
                target_cursor += 1;
                selected_package_roots.push(SelectedPackageRoot::Occurrence(slot.occurrence()));
            }
            SelectedNativeTargetImport::RetainedCapture {
                identity,
                generation,
            } => {
                if identity != &global.identity || global.required_generation != Some(*generation) {
                    return Err(failure(
                        "target capture differs from its compiler-issued global",
                    ));
                }
            }
            SelectedNativeTargetImport::Source { .. } => {}
        }
    }
    if target_cursor != target_links.len() {
        return Err(failure("target package links were not fully consumed"));
    }
    let mut reached = BTreeSet::new();
    let mut pending = selected_package_roots
        .iter()
        .filter_map(|root| match root {
            SelectedPackageRoot::Occurrence(occurrence) => Some(*occurrence),
            SelectedPackageRoot::Held(_) => None,
        })
        .collect::<Vec<_>>();
    while let Some(occurrence) = pending.pop() {
        crate::host_work::checkpoint()?;
        operation
            .charge(1)
            .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
        if reached.insert(occurrence) {
            let row = occurrences
                .get(occurrence as usize)
                .ok_or_else(|| failure("selected package occurrence is out of range"))?;
            pending.extend(row.imports().iter().map(PackageLink::occurrence));
        }
    }
    let mut plans = Vec::with_capacity(reached.len());
    for occurrence in reached {
        let row = &occurrences[occurrence as usize];
        let product = occurrence_products[occurrence as usize];
        let imports = row
            .imports()
            .iter()
            .map(plan_link)
            .collect::<Result<Vec<_>, _>>()?;
        operation
            .reserve::<super::PackageCodePlan>(1)
            .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
        plans.push(super::PackageCodePlan {
            product: Arc::clone(product),
            imports,
        });
    }
    let package_roots = selected_package_roots
        .into_iter()
        .map(|root| match root {
            SelectedPackageRoot::Occurrence(occurrence) => planned(occurrence),
            SelectedPackageRoot::Held(binding) => Ok(super::PackageCodeTarget::Held(binding)),
        })
        .collect::<Result<Vec<_>, _>>()?;
    let packages = super::IssuedPackageCodeGraph::issue(
        sources.acquired_artifacts(),
        plans,
        package_roots,
        &graphs,
        operation,
    )?;
    let mut roots = packages.roots().iter();

    let mut retained = BTreeSet::new();
    let mut close = |links: &[SelectedNativeImport],
                     definitions: DefinitionsView<'_>|
     -> Result<Arc<[CompletedNativeImport]>, CompileError> {
        operation
            .reserve::<CompletedNativeImport>(links.len())
            .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
        links
            .iter()
            .zip(definitions.globals())
            .map(|(link, global)| match link {
                SelectedNativeImport::Source { binding, value } => {
                    if !selected_groups.contains(binding) {
                        return Err(failure("native completion omitted an exact source child"));
                    }
                    Ok(CompletedNativeImport::Source {
                        binding: *binding,
                        value: *value,
                    })
                }
                SelectedNativeImport::Retained {
                    binding,
                    identity,
                    generation,
                } => {
                    retained.insert(super::NativeBindingRequirement {
                        artifact_id: binding.artifact,
                        identity: identity.clone(),
                        generation: *generation,
                    });
                    Ok(CompletedNativeImport::Retained {
                        binding: *binding,
                        identity: identity.clone(),
                        generation: *generation,
                    })
                }
                SelectedNativeImport::Package {
                    interface_digest, ..
                } => {
                    let binding = packages
                        .get(
                            *roots
                                .next()
                                .ok_or_else(|| failure("completed package root is absent"))?,
                        )
                        .ok_or_else(|| failure("completed package root lacks its acquired row"))?;
                    let value = binding.resolve_export(
                        global,
                        definitions.signatures(),
                        interface_digest,
                    )?;
                    Ok(CompletedNativeImport::Package { binding, value })
                }
            })
            .collect::<Result<Vec<_>, _>>()
            .map(Into::into)
    };
    let mut rows = imbl::OrdMap::new();
    for (row, prior, product, _) in row_inputs {
        let imports = close(row.imports(), row.group().group().definitions())?;
        if prior.is_some_and(|prior| prior.imports != imports) {
            return Err(failure(
                "native completion rebound a previously completed HOME row",
            ));
        }
        rows.insert(
            row.binding(),
            Arc::new(CompletedNativeSourceRow {
                binding: row.binding(),
                product,
                imports,
            }),
        );
    }
    drop(close);
    operation
        .reserve::<CompletedNativeTargetImport>(sources.target_requirements().len())
        .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
    let target_imports = sources
        .target_requirements()
        .iter()
        .zip(definitions.globals())
        .map(|(link, global)| match link {
            SelectedNativeTargetImport::Source { binding, value } => {
                if !selected_groups.contains(binding) {
                    return Err(failure("target completion omitted an exact source root"));
                }
                Ok(CompletedNativeTargetImport::Source {
                    binding: *binding,
                    value: *value,
                })
            }
            SelectedNativeTargetImport::RetainedCapture {
                identity,
                generation,
            } => Ok(CompletedNativeTargetImport::RetainedCapture {
                identity: identity.clone(),
                generation: *generation,
            }),
            SelectedNativeTargetImport::Package {
                interface_digest, ..
            } => {
                let binding = packages
                    .get(
                        *roots
                            .next()
                            .ok_or_else(|| failure("target package root is absent"))?,
                    )
                    .ok_or_else(|| failure("target package root lacks its acquired row"))?;
                let value =
                    binding.resolve_export(global, definitions.signatures(), interface_digest)?;
                Ok(CompletedNativeTargetImport::Package { binding, value })
            }
        })
        .collect::<Result<Vec<_>, CompileError>>()?
        .into();
    if roots.next().is_some() {
        return Err(failure(
            "completed package graph has unconsumed target slots",
        ));
    }
    let artifacts = sources
        .acquired_artifacts()
        .retain_custody(packages.artifacts())?;
    Ok(Arc::new(CompletedNativeSelection {
        #[cfg(test)]
        package_selection: selection,
        source_target,
        source_associations,
        completion: IssuedNativeSelectionCompletion::issue_parts(
            *output.producer_sha256(),
            *output.profile_sha256(),
            artifacts,
            rows,
            packages,
            retained.into_iter().collect::<Vec<_>>().into(),
            operation,
        )?,
        target_imports,
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
    native_completions: NativeSelectionCompletions,
    native_offer_facts: imbl::OrdMap<[u8; 32], crate::exact_context::NativeOfferSnapshotFacts>,
    native_availability: crate::declaration_context::RequestNativeAvailability,
    selected: Arc<AdmittedCompilerRoles>,
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
    native_completions: NativeSelectionCompletions,
    native_offer_facts: imbl::OrdMap<[u8; 32], crate::exact_context::NativeOfferSnapshotFacts>,
    native_availability: crate::declaration_context::RequestNativeAvailability,
    selected: Arc<AdmittedCompilerRoles>,
    input_decisions: IssuedInputNativeDecisions,
}

impl SupportDraft {
    /// The only constructor of a usable compiler support. Re-pairs selected
    /// rows against the final acquired entries and maintains the mandatory
    /// native fact owner. Completion custody is already retained in
    /// `artifacts` and is never re-merged here.
    fn close(self) -> Result<CompilerSourceSupport, CompileError> {
        let SupportDraft {
            projection,
            artifacts,
            native_fact_owner,
            native_offers,
            native_completions,
            native_offer_facts,
            native_availability,
            mut selected,
            input_decisions,
        } = self;
        let mut repaired = None;
        for (owner, row) in selected.rows.iter() {
            crate::host_work::checkpoint()?;
            if std::iter::once(&row.interface)
                .chain(row.original.iter())
                .any(|(binding, entry)| {
                    artifacts
                        .lease
                        .payloads
                        .get(&binding.artifact)
                        .is_none_or(|retained| !Arc::ptr_eq(entry, retained))
                })
            {
                let mut retained = row.clone();
                retained.retain_acquisitions(&artifacts)?;
                repaired
                    .get_or_insert_with(|| selected.as_ref().clone())
                    .rows
                    .insert(owner.clone(), retained);
            }
        }
        if let Some(repaired_selected) = repaired {
            selected = Arc::new(repaired_selected);
        }
        let native_availability = native_availability.retain_acquisitions(&artifacts)?;
        let native_fact_owner = crate::exact_context::PublicationNativeFacts::maintain(
            native_fact_owner.as_ref(),
            crate::exact_context::PublicationNativeMaps {
                roles: projection.recorded_roles.clone(),
                offers: native_offer_facts.clone(),
                native_availability: native_availability.persistent_facts().clone(),
                input_decisions: input_decisions.facts.clone(),
            },
        )
        .map_err(|error| failure(&error.to_string()))?;
        Ok(CompilerSourceSupport {
            projection,
            artifacts,
            native_fact_owner,
            native_offers,
            native_completions,
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
    fn append_completion_refs(
        &mut self,
        incoming: &NativeSelectionCompletions,
    ) -> Result<(), CompileError> {
        let completions = self.native_completions.append(incoming)?;
        for completion in incoming.iter() {
            self.artifacts = self.artifacts.retain_custody(completion.artifacts())?;
        }
        self.native_completions = completions;
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
        for completion in self.native_completions.iter() {
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

impl PartialEq for CompilerSourceSupport {
    fn eq(&self, other: &Self) -> bool {
        // Offers are the exact identity/mode/proof/custody set persisted by the
        // existing typed map. Live insertion order is not part of that contract.
        // Borrow its maintained root; comparison never sorts or re-encodes it.
        self.projection == other.projection
            && self.artifacts == other.artifacts
            && self.native_fact_owner == other.native_fact_owner
            && self.native_completions == other.native_completions
            && self.native_offer_facts == other.native_offer_facts
            && self.native_availability == other.native_availability
            && self.selected == other.selected
            && self.input_decisions.recorded_facts() == other.input_decisions.recorded_facts()
    }
}
impl Eq for CompilerSourceSupport {}

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
    selected: imbl::OrdMap<NativeGroupBinding, SelectedNativeSupport>,
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
    pub(super) fn selected_support(
        &self,
        group: NativeGroupBinding,
    ) -> Option<&SelectedNativeSupport> {
        self.selected.get(&group)
    }

    fn with_recorded_facts(
        &self,
        facts: imbl::OrdMap<ExactModuleIdentity, RecordedInputNativeDecision>,
    ) -> Self {
        let roots = facts
            .values()
            .flat_map(|fact| fact.groups.values().copied())
            .collect::<BTreeSet<_>>();
        let mut selected = self.selected.clone();
        let removed = selected
            .keys()
            .filter(|group| !roots.contains(group))
            .copied()
            .collect::<Vec<_>>();
        for group in removed {
            selected.remove(&group);
        }
        for group in roots {
            if !selected.contains_key(&group) {
                selected.insert(group, SelectedNativeSupport::missing());
            }
        }
        Self { facts, selected }
    }

    /// Transfer only this source's selected roots for the same winning role.
    /// Availability and retained custody cannot add an acquisition here.
    pub(super) fn carry_selected_from(&mut self, source: &Self, owner: &ExactModuleIdentity) {
        let Some(fact) = self.facts.get(owner) else {
            return;
        };
        let Some(prior) = source
            .facts
            .get(owner)
            .filter(|prior| prior.role == fact.role)
        else {
            return;
        };
        let groups = fact
            .groups
            .iter()
            .filter_map(|(ordinal, group)| {
                (prior.groups.get(ordinal) == Some(group)).then_some(*group)
            })
            .collect::<Vec<_>>();
        for group in groups {
            let incoming = source
                .selected
                .get(&group)
                .cloned()
                .unwrap_or_else(SelectedNativeSupport::missing);
            if let Some(held) = self.selected.get(&group) {
                let mut merged = held.clone();
                if merged.merge(&incoming) {
                    self.selected.insert(group, merged);
                }
            } else {
                self.selected.insert(group, incoming);
            }
        }
    }

    /// Merge exact selected choices and their inseparable live observations.
    pub(super) fn merge_selected(&mut self, incoming: &Self) -> Result<(), CompileError> {
        *self = self.with_recorded_facts(self.facts.clone());
        for (owner, fact) in &incoming.facts {
            if let Some(previous) = self.facts.get(owner) {
                if previous.role != fact.role {
                    return Err(failure(
                        "selected native decision role differs from its issuer",
                    ));
                }
                let mut groups = previous.groups.clone();
                for (ordinal, group) in &fact.groups {
                    if groups
                        .insert(*ordinal, *group)
                        .is_some_and(|old| old != *group)
                    {
                        return Err(admission_failure(
                            ArtifactInventoryFailure::BindingAmbiguity {
                                artifact: group.binding.artifact(),
                            },
                        ));
                    }
                }
                if previous.groups.diff(&groups).next().is_some() {
                    self.facts.insert(
                        owner.clone(),
                        RecordedInputNativeDecision {
                            role: previous.role.clone(),
                            groups,
                        },
                    );
                }
            } else {
                self.facts.insert(owner.clone(), fact.clone());
            }
            self.carry_selected_from(incoming, owner);
        }
        Ok(())
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
            result.carry_selected_from(&source.input_decisions, owner);
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

/// Exact binding observations retained from successful compiler-role admission.
/// These entries are acquisition owners, not a reconstruction from payload IDs.
#[derive(Debug, Eq, PartialEq)]
pub(super) struct ResolvedCompilerRole {
    pub(super) interface: (ArtifactBinding, Arc<ArtifactEntry>),
    pub(super) original: Option<(ArtifactBinding, Arc<ArtifactEntry>)>,
}

impl Clone for ResolvedCompilerRole {
    fn clone(&self) -> Self {
        #[cfg(test)]
        type_query_work::cloned_row();
        Self {
            interface: self.interface.clone(),
            original: self.original.clone(),
        }
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

impl ResolvedCompilerRole {
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
#[derive(Clone, Debug, Eq, PartialEq)]
struct AdmittedCompilerRoles {
    rows: imbl::OrdMap<ExactModuleIdentity, ResolvedCompilerRole>,
    home_units: Arc<imbl::OrdSet<String>>,
}

impl AdmittedCompilerRoles {
    fn from_admitted(rows: BTreeMap<ExactModuleIdentity, ResolvedCompilerRole>) -> Self {
        let mut home_units = BTreeSet::new();
        for row in rows.values() {
            #[cfg(test)]
            type_query_work::census_row();
            if let ArtifactPayload::Canonical(interface) = &row.interface.1.payload {
                home_units.extend(interface.home_units().iter().cloned());
            }
        }
        Self {
            rows: rows.into_iter().collect(),
            home_units: Arc::new(home_units.into_iter().collect()),
        }
    }

    fn retain_acquisitions(
        &self,
        artifacts: &ArtifactView,
    ) -> Result<imbl::OrdMap<ExactModuleIdentity, ResolvedCompilerRole>, CompileError> {
        self.rows
            .iter()
            .map(|(owner, row)| {
                let mut row = row.clone();
                row.retain_acquisitions(artifacts)?;
                Ok((owner.clone(), row))
            })
            .collect()
    }
}

impl AdmittedCompilerRoles {
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

impl std::ops::Index<&ExactModuleIdentity> for AdmittedCompilerRoles {
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

/// A compiler-only observation issued from this support's acquired selected row.
/// It cannot restore a runtime selection or qualify executable availability.
pub(crate) enum CompilerPrimaryInput<'a> {
    InterfaceOnly(CompilerPrimaryRow<'a>),
    Original(CompilerPrimaryRow<'a>),
}

pub(crate) struct CompilerPrimaryRow<'a> {
    owner: &'a ExactModuleIdentity,
    row: &'a ResolvedCompilerRole,
}

impl<'a> CompilerPrimaryInput<'a> {
    fn row(&self) -> &CompilerPrimaryRow<'a> {
        match self {
            Self::InterfaceOnly(row) | Self::Original(row) => row,
        }
    }
    pub(crate) fn owner(&self) -> &'a ExactModuleIdentity {
        self.row().owner
    }
    pub(crate) fn interface(&self) -> ArtifactBinding {
        self.row().row.interface.0
    }
    pub(crate) fn original(&self) -> Option<ArtifactBinding> {
        self.row()
            .row
            .original
            .as_ref()
            .map(|(binding, _)| *binding)
    }
    pub(crate) fn entry(&self) -> &'a Arc<ArtifactEntry> {
        let row = self.row().row;
        &row.original.as_ref().unwrap_or(&row.interface).1
    }
}

/// Borrowed selected interface observations and the same issuing view that
/// resolves their scoped bindings. Historical custody cannot choose a row.
pub(crate) struct SelectedCompilerInterfaces<'a> {
    artifacts: &'a ArtifactView,
    selected: &'a AdmittedCompilerRoles,
}

enum InterfaceProjectionPurpose {
    PreserveCustody,
    TypeNames,
}

impl SelectedCompilerInterfaces<'_> {
    pub(crate) fn get(&self, owner: &ExactModuleIdentity) -> Option<&Arc<ArtifactEntry>> {
        self.selected.get(owner).map(|row| &row.interface.1)
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (&ExactModuleIdentity, &Arc<ArtifactEntry>)> {
        self.selected
            .iter()
            .map(|(owner, row)| (owner, &row.interface.1))
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
        let mut selected = projection
            .roles
            .keys()
            .map(|owner| {
                let mut row = self.selected[owner].clone();
                row.original = None;
                (owner.clone(), row)
            })
            .collect::<BTreeMap<_, _>>();
        // The rows retain their issuer's exact bindings. Demotion still checks
        // the canonical interface's own dependency closure and seals.
        let entries = selected
            .values()
            .map(|row| (row.interface.0.artifact, Arc::clone(&row.interface.1)))
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
        for row in selected.values_mut() {
            row.retain_acquisitions(&artifacts)?;
        }
        SupportDraft {
            native_availability: Default::default(),
            projection,
            artifacts,
            native_offers: Arc::from([]),
            native_fact_owner: None,
            native_offer_facts: Default::default(),
            native_completions: NativeSelectionCompletions::default(),
            selected: Arc::new(AdmittedCompilerRoles::from_admitted(selected)),
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
            .map(|owner| InventoryNodeKey::Artifact(self.selected[owner].interface.0))
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
            additions
                .into_iter()
                .map(|(source, row)| (source.lease.as_ref(), row.interface.0, &row.interface.1)),
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
            if row.interface.0.artifact != role.interface()
                || &row.interface.1.descriptor.owner != owner
                || !row.interface.1.is_compiler_interface()
                || row.original.as_ref().map(|(binding, _)| binding.artifact) != role.original()
                || row.original.as_ref().is_some_and(|(_, entry)| {
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
            for (original, (binding, entry)) in std::iter::once((false, &row.interface))
                .chain(row.original.iter().map(|entry| (true, entry)))
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
            if let Some((carrier, _)) = &row.original {
                let state = source
                    .artifacts
                    .lease
                    .inventory
                    .0
                    .lock()
                    .expect("inventory lock");
                ResolvedCompilerRole::validate_original_pair(&state, row.interface.0, *carrier)?;
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
                std::iter::once(&row.interface)
                    .chain(row.original.iter())
                    .map(|(_, entry)| entry.as_ref()),
                |required| selected.get(required).map(|row| row.interface.1.as_ref()),
            )?;
            entries.insert(
                owner.clone(),
                Arc::clone(&row.original.as_ref().unwrap_or(&row.interface).1),
            );
        }
        work.selected_rows = selected.len();
        work.metadata_entries = entries.len();
        Ok(DemandCompilerProjection {
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

impl CompilerArtifactMetadata {
    pub(crate) fn retain_acquisitions(
        mut self,
        artifacts: &ArtifactView,
    ) -> Result<Self, CompileError> {
        fn retain<K: Ord + Clone>(
            entries: &mut imbl::OrdMap<K, Arc<ArtifactEntry>>,
            artifacts: &ArtifactView,
        ) -> Result<bool, CompileError> {
            let mut changed = Vec::new();
            for (key, entry) in entries.iter() {
                crate::host_work::checkpoint()?;
                let retained = artifacts
                    .lease
                    .payloads
                    .get(&entry.descriptor.id)
                    .ok_or_else(|| failure("compiler metadata lost its acquired payload owner"))?;
                if !Arc::ptr_eq(entry, retained) {
                    if entry != retained {
                        return Err(admission_failure(
                            ArtifactInventoryFailure::MetadataConflict {
                                artifact: entry.descriptor.id,
                            },
                        ));
                    }
                    changed.push((key.clone(), Arc::clone(retained)));
                }
            }
            let updated = !changed.is_empty();
            for (key, entry) in changed {
                entries.insert(key, entry);
            }
            Ok(updated)
        }
        retain(&mut self.entries, artifacts)?;
        let mut retained = self.retained.as_ref().clone();
        let mut changed = retain(&mut retained.artifacts, artifacts)?;
        changed |= retain(&mut retained.entries, artifacts)?;
        changed |= retain(&mut retained.interface_representatives, artifacts)?;
        if changed {
            self.retained = Arc::new(retained);
        }
        Ok(self)
    }
}

impl CompilerSourceSupport {
    /// Reopen a closed support as assembly state for one bounded mutation.
    /// The mutation must return through [`SupportDraft::close`].
    fn reopen(&self) -> SupportDraft {
        SupportDraft {
            projection: self.projection.clone(),
            artifacts: self.artifacts.clone(),
            native_fact_owner: Some(Arc::clone(&self.native_fact_owner)),
            native_offers: Arc::clone(&self.native_offers),
            native_completions: self.native_completions.clone(),
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
            input_decisions: BTreeSet::new(),
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
            if row.interface.0.artifact != role.interface()
                || row.original.as_ref().map(|(binding, _)| binding.artifact) != role.original()
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
            for (binding, _) in std::iter::once(&row.interface).chain(row.original.iter()) {
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
            // Preserve the exact issued choice domain, including an issued
            // empty domain. Absence remains absence; narrowing grants no choice.
            if matches!(
                self.input_decisions.for_role(owner, role),
                InputNativeDecision::IssuedChoices(_)
            ) {
                charge(operation.reserve::<(ExactModuleIdentity, [usize; 4])>(1))?;
                charge(operation.charge(owner.unit.len().saturating_add(owner.module.len())))?;
                keys.input_decisions.insert(owner.clone());
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
            input_decisions: self
                .input_decisions
                .with_recorded_facts(facts.input_decisions.clone()),
            native_offer_facts: facts.offers.clone(),
            native_completions: self.native_completions.clone(),
            native_fact_owner: Some(facts),
            // The home census belongs to the demanded rows, not to cold
            // custody or units merely mentioned by another retained row.
            selected: Arc::new(AdmittedCompilerRoles::from_admitted(
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
            let ArtifactPayload::Canonical(interface) = &row.interface.1.payload else {
                continue;
            };
            charge(operation.charge(product.interface.len()))?;
            if module.skinny_iface_sha256 != row.interface.1.descriptor.interface_sha256
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
                &row.interface.1.payload
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
            if output.artifact_id() != row.interface.0.artifact
                || interface.interface_bytes() != expected.interface_bytes()
                || interface.package_imports_bytes() != expected.package_imports_bytes()
                || row.interface.1.requirements.as_slice() != output.requirements()
                || returned.interface.sha256 != row.interface.1.descriptor.interface_sha256
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
        Self::new_with_native_facts(
            projection,
            artifacts,
            native_offers,
            native_availability,
            IssuedInputNativeDecisions::default(),
            None,
            NativeSelectionCompletions::default(),
        )
    }

    fn new_with_native_facts(
        projection: CompilerInputProjection,
        artifacts: ArtifactView,
        native_offers: impl IntoIterator<Item = IssuedNativeOffer>,
        native_availability: crate::declaration_context::RequestNativeAvailability,
        input_decisions: IssuedInputNativeDecisions,
        prior: Option<&Arc<crate::exact_context::PublicationNativeFacts>>,
        native_completions: NativeSelectionCompletions,
    ) -> Result<Self, CompileError> {
        let mut timing = crate::timing::InvocationTiming::start_scoped(
            "exact.compiler_support.new",
            "validate_projection",
        );
        projection.validate(&artifacts)?;
        timing.advance("resolve_associations");
        let selected = projection.resolve_associations(&artifacts)?;
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
            native_completions,
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
        native_completions: NativeSelectionCompletions,
        timing: &mut crate::timing::InvocationTiming,
    ) -> Result<SupportDraft, CompileError> {
        let ResolvedCompilerInputs {
            artifacts,
            projection,
            mut selected,
            input_decisions,
        } = inputs;
        let artifacts = native_completions
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
                if !offers.contains(&offer) {
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
        timing.advance("retain_selected_acquisitions");
        for row in selected.values_mut() {
            row.retain_acquisitions(&retained)?;
        }
        timing.advance("qualify_canonical_availability");
        let native_availability = native_availability.for_compiler_roles(&projection, &retained)?;
        timing.advance("assemble_support");
        let input_decisions = input_decisions.with_recorded_facts(input_decisions.facts.clone());
        let support = SupportDraft {
            native_availability,
            projection,
            artifacts: retained,
            native_fact_owner: prior.cloned(),
            native_offer_facts,
            native_completions,
            native_offers: offers.into(),
            selected: Arc::new(AdmittedCompilerRoles::from_admitted(selected)),
            input_decisions,
        };
        Ok(support)
    }

    fn rebuild(
        &self,
        projection: CompilerInputProjection,
        artifacts: ArtifactView,
        offers: impl IntoIterator<Item = IssuedNativeOffer>,
    ) -> Result<Self, CompileError> {
        let decisions = self.input_decisions.for_exact_roles(&projection);
        Self::new_with_native_facts(
            projection,
            artifacts,
            offers,
            self.native_availability.clone(),
            decisions.clone(),
            Some(&self.native_fact_owner),
            self.native_completions.clone(),
        )
    }

    fn rebuild_narrowed(
        &self,
        projection: CompilerInputProjection,
        artifacts: ArtifactView,
        offers: impl IntoIterator<Item = IssuedNativeOffer>,
    ) -> Result<Self, CompileError> {
        let decisions = self.input_decisions.restricted(&projection, &artifacts);
        Self::new_with_native_facts(
            projection,
            artifacts,
            offers,
            self.native_availability.clone(),
            decisions.clone(),
            Some(&self.native_fact_owner),
            self.native_completions.clone(),
        )
    }

    fn with_additional_decisions(
        &self,
        incoming: Option<&IssuedInputNativeDecisions>,
    ) -> Result<Self, CompileError> {
        let mut draft = self.reopen();
        if let Some(incoming) = incoming {
            let merged = incoming.for_exact_roles(&draft.projection);
            draft.input_decisions.merge_selected(&merged)?;
        }
        draft.close()
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
                let offer = IssuedNativeOffer::issue(original, &artifacts, proof)?;
                for group in groups.values().copied() {
                    input_decisions.selected.insert(
                        group,
                        SelectedNativeSupport::Acquired(Arc::from([offer.clone()])),
                    );
                }
                input_decisions.facts.insert(
                    owner,
                    RecordedInputNativeDecision {
                        role: role.recorded_fact(),
                        groups,
                    },
                );
                offers.push(offer);
            }
        }
        Self::new_with_native_facts(
            projection.clone(),
            artifacts.clone(),
            offers.clone(),
            crate::declaration_context::RequestNativeAvailability::issue_checked(
                &projection,
                &artifacts,
                &offers,
            )?,
            input_decisions,
            None,
            NativeSelectionCompletions::default(),
        )
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
        for (binding, entry) in std::iter::once(&row.interface).chain(row.original.iter()) {
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
                    .payloads
                    .get(&binding.artifact)
                    .is_none_or(|held| !Arc::ptr_eq(held, entry))
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
        if let Some((original, _)) = row.original.as_ref() {
            let state = self
                .artifacts
                .lease
                .inventory
                .0
                .lock()
                .expect("inventory lock");
            ResolvedCompilerRole::validate_original_pair(&state, row.interface.0, *original)?;
            Ok(CompilerPrimaryInput::Original(CompilerPrimaryRow {
                owner,
                row,
            }))
        } else {
            Ok(CompilerPrimaryInput::InterfaceOnly(CompilerPrimaryRow {
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
        let transport = row.original.as_ref().unwrap_or(&row.interface);
        Some(WorldPrimaryTypeRow {
            owner,
            interface_binding: &row.interface.0,
            original_binding: row.original.as_ref().map(|(binding, _)| binding),
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
                if producer == [0; 32] || row.interface.1.descriptor.producer_sha256 != producer {
                    return Err(failure(
                        "type-interface row differs from the primary compiler producer",
                    ));
                }
                let role = &support.projection.roles[owner];
                if !matches!(role, CompilerInputRole::InterfaceOnly { .. })
                    || row.original.is_some()
                {
                    return Err(failure("type-interface delta contains an original role"));
                }
                CompilerInputProjection::validate_role_payload(
                    owner,
                    role,
                    &row.interface.1,
                    |_| None,
                )?;
                if let Some(prior) = selected.get(owner) {
                    if prior.interface.0.artifact != row.interface.0.artifact {
                        return Err(admission_failure(ArtifactInventoryFailure::OwnerConflict {
                            owner: owner.clone(),
                        }));
                    }
                    if prior.interface.0 != row.interface.0 {
                        return Err(admission_failure(
                            ArtifactInventoryFailure::BindingAmbiguity {
                                artifact: row.interface.0.artifact,
                            },
                        ));
                    }
                    continue;
                }
                projection.admit_role(owner.clone(), role.clone())?;
                if let ArtifactPayload::Canonical(interface) = &row.interface.1.payload {
                    home_units.extend(interface.home_units().iter().cloned());
                }
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
                    .map(|(_, row)| row.interface.1.as_ref()),
                |owner| selected.get(owner).map(|row| row.interface.1.as_ref()),
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
                actual.interface.0 == expected.interface.0
                    && actual.interface.0.artifact == interface
            }
            _ => false,
        }
    }

    pub(crate) fn implementation_entry(
        &self,
        owner: &ExactModuleIdentity,
    ) -> Option<&Arc<ArtifactEntry>> {
        self.selected.get(owner).map(|row| {
            row.original
                .as_ref()
                .map_or(&row.interface.1, |(_, entry)| entry)
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
                promotion |= previous.original.is_none() && row.original.is_some();
                if previous.interface.0.artifact != row.interface.0.artifact {
                    return Err(admission_failure(ArtifactInventoryFailure::OwnerConflict {
                        owner: owner.clone(),
                    }));
                }
                if previous.interface.0 != row.interface.0 {
                    tracing::debug!(
                        target: "tidepool::artifact_inventory::ambiguity_trace",
                        ?owner,
                        previous_binding = ?previous.interface.0,
                        incoming_binding = ?row.interface.0,
                        previous_role = ?self.projection.roles[owner],
                        incoming_role = ?role,
                        receiving_caller = %std::panic::Location::caller(),
                        "program fragment carries different issued interface bindings"
                    );
                    return Err(admission_failure(
                        ArtifactInventoryFailure::BindingAmbiguity {
                            artifact: row.interface.0.artifact,
                        },
                    ));
                }
                if let (Some((old, _)), Some((new, _))) = (&previous.original, &row.original) {
                    if old != new {
                        return Err(admission_failure(
                            ArtifactInventoryFailure::BindingAmbiguity {
                                artifact: new.artifact,
                            },
                        ));
                    }
                }
            }
            projection.admit_role(owner.clone(), role.clone())?;
            let effective = &projection.roles[owner];
            let mut retained = row.clone();
            if effective.original().is_some() && retained.original.is_none() {
                retained.original = selected.get(owner).and_then(|prior| prior.original.clone());
            }
            if selected.get(owner) != Some(&retained) {
                if let ArtifactPayload::Canonical(interface) = &row.interface.1.payload {
                    home_units.extend(interface.home_units().iter().cloned());
                }
                selected.rows.insert(owner.clone(), retained);
                changed.insert(owner.clone());
            }
        }
        projection.validate_requirements(
            incoming.selected.iter().flat_map(|(_, row)| {
                std::iter::once(&row.interface)
                    .chain(row.original.iter())
                    .map(|(_, entry)| entry.as_ref())
            }),
            |owner| selected.get(owner).map(|row| row.interface.1.as_ref()),
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
                continue;
            }
            native_offer_facts.insert(id, offer.snapshot_facts());
            new_offers.push(offer.clone());
            offers.push(offer.clone());
        }
        let artifacts = self.artifacts.append_admitted_custody(
            incoming.selected.iter().flat_map(|(_, row)| {
                std::iter::once(&row.interface)
                    .chain(row.original.iter())
                    .map(|(binding, entry)| (incoming.artifacts.lease.as_ref(), *binding, entry))
            }),
            &new_offers,
        )?;
        // The draft invariant owns completion custody in its artifacts before
        // the draft exists; self's completions stay retained through the
        // append, and incoming completions merge here exactly once.
        let artifacts = incoming
            .native_completions
            .iter()
            .try_fold(artifacts, |view, completion| {
                view.retain_custody(completion.artifacts())
            })?;
        for (owner, _) in incoming.selected.iter() {
            let row = selected.rows.get_mut(owner).expect("incoming admitted row");
            let interface = &artifacts.lease.payloads[&row.interface.0.artifact];
            let mut custody_changed = !Arc::ptr_eq(&row.interface.1, interface);
            row.interface.1 = Arc::clone(interface);
            if let Some((binding, entry)) = &mut row.original {
                let retained = &artifacts.lease.payloads[&binding.artifact];
                custody_changed |= !Arc::ptr_eq(entry, retained);
                *entry = Arc::clone(retained);
            }
            if custody_changed {
                changed.insert(owner.clone());
            }
        }
        let mut input_decisions = self.input_decisions.clone();
        input_decisions.merge_selected(&incoming.input_decisions.for_exact_roles(&projection))?;
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
                native_availability.for_compiler_roles(&projection, &artifacts)?;
        }
        selected.home_units = Arc::new(home_units);
        Ok(ProgramSupportDelta {
            support: SupportDraft {
                projection,
                artifacts,
                native_fact_owner: Some(Arc::clone(&self.native_fact_owner)),
                native_offers: offers.into(),
                native_offer_facts,
                native_completions: self
                    .native_completions
                    .append(&incoming.native_completions)?,
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
                Arc::clone(&row.original.as_ref().unwrap_or(&row.interface).1),
            );
        }
        let mut retained = prior.retained.as_ref().clone();
        let mut bindings = BTreeSet::new();
        for (_, row) in incoming.selected.iter() {
            bindings.insert(row.interface.0);
            bindings.extend(row.original.iter().map(|(binding, _)| *binding));
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
        CompilerArtifactMetadata {
            entries,
            retained: Arc::new(retained),
        }
        .retain_acquisitions(&self.artifacts)
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
            let (binding, entry) = &row.interface;
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
        CompilerArtifactMetadata {
            entries,
            retained: Arc::new(retained),
        }
        .retain_acquisitions(&self.artifacts)
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
        let (_, entry) = admitted.original.as_ref().ok_or_else(|| {
            InputDecisionRefusal::ArtifactIdentityMismatch(ArtifactIdentityMismatch {
                expected_original: Some(original),
                expected_canonical: Some(canonical),
                ..ArtifactIdentityMismatch::refused(ArtifactIdentityStage::MissingOriginalSlot)
            })
        })?;
        if selected_original != original
            || entry.descriptor.id != original
            || role.interface() != canonical
            || admitted.interface.1.descriptor.id != canonical
        {
            return Err(InputDecisionRefusal::ArtifactIdentityMismatch(
                ArtifactIdentityMismatch {
                    stage: ArtifactIdentityStage::RequestedIdentityDiffers,
                    expected_original: Some(original),
                    expected_canonical: Some(canonical),
                    observed_original: Some(entry.descriptor.id),
                    observed_canonical: Some(admitted.interface.1.descriptor.id),
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
                    observed_canonical: Some(admitted.interface.1.descriptor.id),
                    observed_kind: Some(entry.payload.artifact_kind()),
                    ..ArtifactIdentityMismatch::refused(ArtifactIdentityStage::NonOriginalPayload)
                },
            ));
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
            .for_compiler_roles(&self.projection, &original.artifacts)?;
        timing.advance("collect_offers");
        let mut offers = Vec::new();
        for row in availability.rows() {
            for offer in row.offers() {
                if !offers.contains(offer) {
                    offers.push(offer.clone());
                }
            }
        }
        timing.advance("retain_offer_custody");
        let retained = self
            .artifacts
            .retain_issued_offer_custody(offers.iter().cloned())?;
        timing.advance("construct_support");
        let support = Self::new_with_native_facts(
            self.projection.clone(),
            retained,
            offers,
            availability,
            self.input_decisions.clone(),
            Some(&self.native_fact_owner),
            self.native_completions.clone(),
        )?;
        timing.advance("carry_input_decisions");
        let result = support.with_additional_decisions(Some(&original.input_decisions));
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
                    rows.for_compiler_roles(&self.projection, &self.artifacts)?;
                draft.close()
            }
        }
    }

    pub(crate) fn restore_native_completion_facts(
        &self,
        facts: &imbl::OrdMap<[u8; 32], Arc<NativeCompletionSnapshotFacts>>,
        validation: &mut crate::recovery_artifacts::PackageInterfaceValidation,
    ) -> Result<Self, CompileError> {
        let mut restored = NativeSelectionCompletions::default();
        for (identity, recorded) in facts {
            crate::host_work::checkpoint()?;
            if recorded.identity_in(Some(&validation.inventory))? != *identity {
                return Err(failure(
                    "native completion fact commitment differs from its record",
                ));
            }
            let completion = restore_native_completion(recorded, &self.artifacts, validation)?;
            restored = restored.append(&NativeSelectionCompletions::one(completion))?;
        }
        // Restoration replaces the recorded completion set; receiver facts
        // cannot fill missing records in the requested snapshot.
        let mut draft = self.reopen();
        draft.native_completions = NativeSelectionCompletions::default();
        draft.append_completion_refs(&restored)?;
        draft.close()
    }

    pub(crate) fn native_completion_facts(
        &self,
    ) -> &imbl::OrdMap<[u8; 32], Arc<NativeCompletionSnapshotFacts>> {
        self.native_completions.recorded_facts()
    }

    pub(crate) fn native_completions(&self) -> &NativeSelectionCompletions {
        &self.native_completions
    }

    /// Import only already-issued immutable completion custody. Compiler roles,
    /// input decisions and availability stay with the receiving owner.
    pub(crate) fn with_native_selection_completion(
        &self,
        completed: &CompletedNativeSelection,
    ) -> Result<Self, CompileError> {
        let mut draft = self.reopen();
        draft.append_completion_refs(&NativeSelectionCompletions::one(Arc::clone(
            completed.completion(),
        )))?;
        draft.close()
    }

    pub(crate) fn with_native_completions_from(
        &self,
        incoming: &Self,
    ) -> Result<Self, CompileError> {
        let mut draft = self.reopen();
        draft.append_completion_refs(&incoming.native_completions)?;
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
        let availability = match fragment {
            Some(fragment) => self
                .native_availability
                .merge(fragment.native_availability())?
                .into_rows(),
            None => self.native_availability.clone(),
        };
        let completions = match fragment {
            Some(fragment) => self
                .native_completions
                .append(fragment.issued_native_completions())?,
            None => self.native_completions.clone(),
        };
        let support = Self::new_with_native_facts(
            projection,
            artifacts,
            offers,
            availability,
            decisions.clone(),
            Some(&self.native_fact_owner),
            completions,
        )?;
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
            let resolved = Self::new_with_native_facts(
                projection,
                artifacts,
                support.native_offers.iter().cloned(),
                support.native_availability.clone(),
                decisions.clone(),
                Some(&support.native_fact_owner),
                support.native_completions.clone(),
            )?;
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
                let support = Self::from_selected_issuance(projection, artifacts)?;
                let mut draft = support.reopen();
                draft.input_decisions = IssuedInputNativeDecisions::default()
                    .with_recorded_facts(draft.input_decisions.facts.clone());
                draft.close()
            }
            crate::declaration_context::RecoveredCompilerIssuance::RecordedRoles => Self::new(
                projection.bind_publications(&artifacts)?,
                artifacts,
                [],
                Default::default(),
            ),
        }
    }

    pub(crate) fn restore_snapshot_evidence(
        &self,
        snapshot: &crate::exact_context::ExactContextSnapshot,
    ) -> Result<Self, CompileError> {
        if snapshot
            .native_completions
            .values()
            .any(|facts| facts.producer_sha256 != snapshot.producer)
        {
            return Err(failure(
                "native completion belongs to another context producer",
            ));
        }
        let restored = self.restore_native_maps(
            &snapshot.roles,
            &snapshot.offers,
            &snapshot.native_availability,
            &snapshot.input_decisions,
        )?;
        restored.restore_native_completion_facts(
            &snapshot.native_completions,
            &mut crate::recovery_artifacts::PackageInterfaceValidation::default(),
        )
    }

    fn restore_native_maps(
        &self,
        roles: &imbl::OrdMap<ExactModuleIdentity, RecordedCompilerRole>,
        offer_facts: &imbl::OrdMap<[u8; 32], crate::exact_context::NativeOfferSnapshotFacts>,
        availability_facts: &imbl::OrdMap<
            ArtifactId,
            crate::declaration_context::NativeAvailabilitySnapshotFacts,
        >,
        decision_facts: &imbl::OrdMap<ExactModuleIdentity, RecordedInputNativeDecision>,
    ) -> Result<Self, CompileError> {
        if self.projection.recorded_roles() != roles {
            return Err(failure(
                "recovered availability roles differ from their context",
            ));
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
        let artifacts = self
            .artifacts
            .retain_issued_offer_custody(offers.iter().cloned())?;
        let availability = crate::declaration_context::RequestNativeAvailability::restore(
            availability_facts,
            &proof_map,
            &artifacts,
        )?;
        let restored = Self::new(self.projection.clone(), artifacts, offers, availability)?;
        restored
            .native_availability
            .validate_role_coverage(&restored.projection)?;
        let state = restored
            .artifacts
            .lease
            .inventory
            .0
            .lock()
            .expect("inventory lock");
        for (owner, fact) in decision_facts {
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
        draft.input_decisions =
            IssuedInputNativeDecisions::default().with_recorded_facts(decision_facts.clone());
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
        Self::new_with_native_facts(
            output.role_projection().clone(),
            output.issued_artifacts().clone(),
            output.issued_offers().iter().cloned(),
            output.native_availability().clone(),
            output.issued_input_decisions().cloned().unwrap_or_default(),
            output.native_fact_base(),
            output.issued_native_completions().clone(),
        )
        .and_then(|support| support.with_additional_decisions(output.issued_input_decisions()))
    }

    pub(crate) fn from_original_output(
        compiler: &Self,
        parent: &Self,
        output: &crate::declaration_context::OriginalCompilerInputs,
    ) -> Result<Self, CompileError> {
        let projection = compiler
            .projection()
            .within_view(output.issued_artifacts())
            .merge(output.role_projection())?;
        let rebuilt = compiler.rebuild(
            projection,
            output.issued_artifacts().clone(),
            parent
                .native_offers()
                .iter()
                .chain(output.issued_offers())
                .cloned(),
        )?;
        let retained = rebuilt.retain_normalized_availability(output.native_availability())?;
        let decided = retained.with_additional_decisions(output.issued_input_decisions())?;
        let mut draft = decided.reopen();
        draft.append_completion_refs(output.issued_native_completions())?;
        draft.append_completion_refs(parent.native_completions())?;
        draft.close()
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
        let generated = source.generated_source_owner()?;
        let mut owners = source.home_imports()?.into_keys().collect::<BTreeSet<_>>();
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
        let selected = projection.resolve_source_associations(artifacts)?;
        projection.validate_requirements(
            selected.values().flat_map(|row| {
                std::iter::once(&row.interface)
                    .chain(row.original.iter())
                    .map(|(_, entry)| entry.as_ref())
            }),
            |owner| selected.get(owner).map(|row| row.interface.1.as_ref()),
        )?;
        let input_decisions = inputs
            .issued_input_decisions()
            .cloned()
            .unwrap_or_default()
            .for_exact_roles(&projection);
        let native_availability = inputs
            .native_availability()
            .for_compiler_roles(&projection, artifacts)?;
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
            .issued_native_completions()
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
            native_completions: inputs.issued_native_completions().clone(),
            native_availability,
            selected: Arc::new(AdmittedCompilerRoles::from_admitted(selected)),
            input_decisions,
        }
        .close()
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
            .flat_map(|availability| availability.offers().cloned());
        let incoming_rows =
            crate::declaration_context::RequestNativeAvailability::from_issued_rows(
                selection.issued_native_availability().cloned(),
            )?;
        let availability = match self.native_availability.merge(&incoming_rows)? {
            crate::declaration_context::NativeAvailabilityMerge::Unchanged(rows)
            | crate::declaration_context::NativeAvailabilityMerge::Changed(rows) => rows,
        };
        Self::new_with_native_facts(
            projection,
            artifacts,
            self.native_offers.iter().cloned().chain(offers),
            availability,
            decisions,
            Some(&self.native_fact_owner),
            self.native_completions
                .append(&selection.native_completions())?,
        )
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
        let selected = self.selected.retain_acquisitions(&artifacts)?;
        // Custody attachment preserves the exact namespace and executable
        // roots. Its acquisition owners may change; its qualified rows cannot.
        SupportDraft {
            projection: self.projection.clone(),
            artifacts,
            native_offers: Arc::clone(&self.native_offers),
            native_fact_owner: Some(Arc::clone(&self.native_fact_owner)),
            native_offer_facts: self.native_offer_facts.clone(),
            native_completions: self.native_completions.clone(),
            native_availability: self.native_availability.clone(),
            selected: Arc::new(AdmittedCompilerRoles {
                rows: selected,
                home_units: Arc::clone(&self.selected.home_units),
            }),
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
                    if offers.contains(&offer) {
                        None
                    } else {
                        offers.push(offer.clone());
                        Some(offer)
                    }
                }),
        )?;
        let selected = self.selected.retain_acquisitions(&artifacts)?;
        // Raw offers add checked custody only. They cannot change the admitted
        // namespace, normalized permissions or recorded execution decisions.
        SupportDraft {
            projection: self.projection.clone(),
            artifacts,
            native_fact_owner: Some(Arc::clone(&self.native_fact_owner)),
            native_offer_facts,
            native_completions: self.native_completions.clone(),
            native_offers: offers.into(),
            native_availability: self.native_availability.clone(),
            selected: Arc::new(AdmittedCompilerRoles {
                rows: selected,
                home_units: Arc::clone(&self.selected.home_units),
            }),
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
        self.rebuild(projection, artifacts, self.native_offers.iter().cloned())
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
            for (_, entry) in std::iter::once(&row.interface).chain(row.original.iter()) {
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
                    ));
                }
            }
        }
        let input_decisions = self.input_decisions.for_exact_roles(&projection);
        SupportDraft {
            native_availability: self
                .native_availability
                .for_compiler_roles(&projection, &self.artifacts)?,
            projection,
            artifacts: self.artifacts.clone(),
            native_offers: Arc::clone(&self.native_offers),
            native_fact_owner: Some(Arc::clone(&self.native_fact_owner)),
            native_offer_facts: self.native_offer_facts.clone(),
            native_completions: self.native_completions.clone(),
            selected: Arc::new(AdmittedCompilerRoles {
                rows: selected,
                home_units: Arc::clone(&self.selected.home_units),
            }),
            input_decisions,
        }
        .close()
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
        let produced = originals.produced_availability();
        let availability = if let Some(receipt) = produced {
            match self.native_availability.merge(
                &crate::declaration_context::RequestNativeAvailability::from_issued_rows([
                    Arc::clone(receipt.row()),
                ])?,
            )? {
                crate::declaration_context::NativeAvailabilityMerge::Unchanged(rows)
                | crate::declaration_context::NativeAvailabilityMerge::Changed(rows) => rows,
            }
        } else {
            self.native_availability.clone()
        };
        Self::new_with_native_facts(
            projection,
            artifacts,
            self.native_offers.iter().cloned().chain(
                produced
                    .into_iter()
                    .map(|receipt| receipt.own_offer().clone()),
            ),
            availability,
            decisions,
            Some(&self.native_fact_owner),
            self.native_completions.clone(),
        )
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
                        ResolvedCompilerRole {
                            interface,
                            original: None,
                        },
                    );
                }
                // The inventory appended only interface rows with checked
                // targets. Earlier exact namespace bindings, scopes, roots and
                // acquisitions remain owned by the same parent snapshot.
                let draft = SupportDraft {
                    projection: appended.projection,
                    artifacts: appended.artifacts,
                    selected: Arc::new(AdmittedCompilerRoles {
                        rows,
                        home_units: Arc::clone(&self.selected.home_units),
                    }),
                    native_offers: Arc::clone(&self.native_offers),
                    native_fact_owner: Some(Arc::clone(&self.native_fact_owner)),
                    native_offer_facts: self.native_offer_facts.clone(),
                    native_completions: self.native_completions.clone(),
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
            entries,
            &self.native_offers,
            &catalog,
        )?;
        timing.advance("rebuild_support");
        let result = self.rebuild(projection, artifacts, self.native_offers.iter().cloned());
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
        let Some(CompilerInputRole::PublishedSourceOriginal {
            native_authority,
            selection,
            ..
        }) = self.projection.snapshot_roles().get(root)
        else {
            return Err(failure(
                "publication birth lacks its acquired published role",
            ));
        };
        let saved = native_authority.acquired()?;
        if saved.artifacts.selection_snapshot().as_ref() != selection.snapshot().as_ref() {
            return Err(failure(
                "publication native owner differs from its issued selection",
            ));
        }
        Ok(SupportDraft {
            projection: saved.projection.clone(),
            artifacts: saved.artifacts.clone(),
            native_fact_owner: Some(Arc::clone(&saved.facts)),
            native_offers: Arc::clone(&saved.offers),
            native_offer_facts: saved.facts.offers.clone(),
            native_completions: saved.completions.clone(),
            native_availability: saved.availability.clone(),
            selected: Arc::clone(&saved.selected),
            input_decisions: saved.decisions.clone(),
        }
        .close()?)
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
        let birth = match &native_authority.0 {
            PublicationNativeState::Acquired(birth) => birth,
            PublicationNativeState::Recorded(_) => {
                return Err(admission_failure(
                    ArtifactInventoryFailure::PublicationNativeDomainMismatch {
                        owner: endorsement.owner().clone(),
                    },
                ));
            }
        };
        // The exact fact domain and the acquired admission owners are separate
        // obligations. Equal IDs/graphs cannot substitute another birth's mode.
        if self.native_fact_owner().commitment() != birth.facts.commitment()
            || !Arc::ptr_eq(&self.selected, &birth.selected)
            || !Arc::ptr_eq(&self.artifacts.lease, &birth.artifacts.lease)
            || !Arc::ptr_eq(&self.native_offers, &birth.offers)
            || !self
                .native_availability
                .same_acquisition(&birth.availability)
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
        let previous = self.validate_publication_endorsement(endorsement)?;
        let role = endorsement.role();
        let mut projection = self.projection.clone();
        projection.admit_role(endorsement.owner().clone(), role.clone())?;
        let mut decisions = self.input_decisions.clone();
        if let Some(fact) = decisions
            .facts
            .get_mut(endorsement.owner())
            .filter(|fact| &fact.role == previous)
        {
            // The checked endorsement transfers the SAME chosen acquisition
            // while changing its role; ordinary role pruning cannot do this.
            fact.role = role.recorded_fact();
        }
        let decisions = decisions.for_exact_roles(&projection);
        Self::new_with_native_facts(
            projection,
            self.artifacts.clone(),
            self.native_offers.iter().cloned(),
            self.native_availability.clone(),
            decisions,
            Some(&self.native_fact_owner),
            self.native_completions.clone(),
        )
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
        let mut projection = CompilerInputProjection::default();
        let mut selected = BTreeMap::new();
        let mut pending = owners
            .iter()
            .filter(|owner| self.projection.roles.contains_key(*owner))
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
                row.original = None;
            }
            let entry = row.original.as_ref().unwrap_or(&row.interface);
            pending.extend(entry.1.requirements.iter().cloned());
            selected.insert(owner.clone(), row);
            projection.admit_role(owner, role)?;
        }
        let entries = selected
            .values()
            .flat_map(|row| std::iter::once(&row.interface).chain(row.original.iter()))
            .map(|(binding, entry)| (binding.artifact, Arc::clone(entry)))
            .collect();
        projection.entries_from_artifacts(&entries)?;
        // Keep the existing topology/custody transition. Only its already
        // authenticated role rows bypass metadata reconstruction afterward.
        let artifacts = self.artifacts.select_compiler_support(&projection)?;
        let mut native_offer_facts = self.native_offer_facts.clone();
        let offers = self
            .native_offers
            .iter()
            .filter_map(|offer| match offer.is_relevant_to_support(&projection) {
                Ok(true) => Some(Ok(offer.clone())),
                Ok(false) => match offer.snapshot_identity() {
                    Ok(identity) => {
                        native_offer_facts.remove(&identity);
                        None
                    }
                    Err(error) => Some(Err(error)),
                },
                Err(error) => Some(Err(error)),
            })
            .collect::<Result<Vec<_>, _>>()?;
        let decisions = self.input_decisions.restricted(&projection, &artifacts);
        let artifacts = artifacts.retain_issued_offer_custody(offers.iter().cloned())?;
        for row in selected.values_mut() {
            row.retain_acquisitions(&artifacts)?;
        }
        SupportDraft {
            native_availability: self
                .native_availability
                .for_compiler_roles(&projection, &self.artifacts)?,
            projection,
            artifacts,
            native_fact_owner: Some(Arc::clone(&self.native_fact_owner)),
            native_offer_facts,
            native_completions: self.native_completions.clone(),
            native_offers: offers.into(),
            selected: Arc::new(AdmittedCompilerRoles::from_admitted(selected)),
            input_decisions: decisions,
        }
        .retain_runtime_custody(&self.artifacts)?
        .close()
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
        // Merged custody preserves the checked payload facts; only its acquisition
        // owner can change. Refresh those Arcs once, after offer retention below.
        inputs
            .projection
            .validate_resolved(&inputs.selected, &inputs.artifacts)?;
        let support = Self::complete_resolved(
            inputs,
            self.native_offers
                .iter()
                .chain(other.native_offers.iter())
                .cloned(),
            self.native_availability.clone(),
            Some(&self.native_fact_owner),
            self.native_completions.append(&other.native_completions)?,
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
    /// Acquire durable role facts against the exact view that must supply their
    /// payloads. The recorded owner key is authoritative; a role whose
    /// interface resolves to another owner is not admitted.
    pub(crate) fn acquire_recorded(
        view: &ArtifactView,
        recorded_roles: &imbl::OrdMap<ExactModuleIdentity, RecordedCompilerRole>,
    ) -> Result<Self, CompileError> {
        let metadata = view.metadata_snapshot();
        let mut projection = Self::default();
        for (owner, fact) in recorded_roles {
            let role = fact.to_recorded_role();
            if metadata
                .artifacts
                .get(&role.interface())
                .is_none_or(|entry| &entry.descriptor.owner != owner)
            {
                return Err(failure(
                    "recorded compiler role differs from its retained owner",
                ));
            }
            projection.admit_role(owner.clone(), role)?;
        }
        projection.validate(view)?;
        Ok(projection)
    }

    fn bind_publications(&self, artifacts: &ArtifactView) -> Result<Self, CompileError> {
        let mut bound = self.clone();
        for (owner, role) in &self.roles {
            if let CompilerInputRole::PublishedSourceOriginal {
                native_authority, ..
            } = role
            {
                let authority = native_authority.bind(artifacts, self, owner)?;
                let mut role = role.clone();
                if let CompilerInputRole::PublishedSourceOriginal {
                    native_authority, ..
                } = &mut role
                {
                    *native_authority = authority;
                }
                bound.roles.insert(owner.clone(), role);
            }
        }
        Ok(bound)
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
        CompilerSourceSupport::new(projection, artifacts, [], Default::default())
    }

    pub(crate) fn snapshot_roles(&self) -> &imbl::OrdMap<ExactModuleIdentity, CompilerInputRole> {
        &self.roles
    }
    pub(crate) fn recorded_roles(
        &self,
    ) -> &imbl::OrdMap<ExactModuleIdentity, RecordedCompilerRole> {
        &self.recorded_roles
    }
    pub(crate) fn bind_snapshot_roles(
        &mut self,
        roles: &imbl::OrdMap<ExactModuleIdentity, RecordedCompilerRole>,
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
                        .insert(owner.clone(), role.recorded_fact());
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
                    ArtifactEntry::canonical_artifact_id(
                        product
                            .module_interface()
                            .ok_or_else(|| failure("native compiler role lacks its interface"))?,
                    ),
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
                    native_refusal: None,
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
            self.roles.insert(owner.clone(), incoming);
        }
        let role = self.roles.get(&owner).expect("admitted role");
        if self
            .recorded_roles
            .get(&owner)
            .is_none_or(|fact| fact != role)
        {
            self.recorded_roles.insert(owner, role.recorded_fact());
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

    fn validate_resolved(
        &self,
        selected: &BTreeMap<ExactModuleIdentity, ResolvedCompilerRole>,
        view: &ArtifactView,
    ) -> Result<(), CompileError> {
        for (owner, role) in &self.roles {
            #[cfg(test)]
            type_query_work::canonical_check();
            let row = selected.get(owner).expect("merged role has its chosen row");
            if row.interface.0.artifact != role.interface()
                || row.interface.1.descriptor.id != role.interface()
                || row.original.as_ref().map(|(binding, _)| binding.artifact) != role.original()
            {
                return Err(failure("merged compiler row differs from its chosen role"));
            }
            Self::validate_role_payload(owner, role, &row.interface.1, |id| {
                row.original
                    .as_ref()
                    .map(|(_, entry)| entry)
                    .filter(|entry| entry.descriptor.id == id)
            })?;
        }
        self.validate_requirements(
            selected
                .values()
                .map(|row| row.original.as_ref().unwrap_or(&row.interface).1.as_ref()),
            |owner| selected.get(owner).map(|row| row.interface.1.as_ref()),
        )?;
        let state = view.lease.inventory.0.lock().expect("inventory lock");
        for row in selected.values() {
            if let Some((carrier, _)) = &row.original {
                ResolvedCompilerRole::validate_original_pair(&state, row.interface.0, *carrier)?;
            }
        }
        Ok(())
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

    /// The checked source issuer supplies exact role IDs. Its authenticated
    /// namespace already owns those bindings; absent namespace entries keep
    /// the existing unique-acquisition fallback and ambiguity refusal.
    fn resolve_source_associations(
        &self,
        view: &ArtifactView,
    ) -> Result<BTreeMap<ExactModuleIdentity, ResolvedCompilerRole>, CompileError> {
        let artifacts = CompilerRolePayloads::PersistentMetadata(&view.lease.payloads);
        let state = view.lease.inventory.0.lock().expect("inventory lock");
        self.resolve_associations_from(&artifacts, &state, |id| {
            match view.lease.selection.namespace.get(&id) {
                Some(binding) => Ok(*binding),
                None => view.resolve_binding(id, &view.read_projection(&state).nodes),
            }
        })
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
                ResolvedCompilerRole {
                    interface: (
                        canonical,
                        Arc::clone(
                            artifacts
                                .get(&role.interface())
                                .expect("associated canonical interface"),
                        ),
                    ),
                    original,
                },
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
                let canonical = product
                    .module_interface()
                    .ok_or_else(|| failure("compiler original offer lacks canonical attachment"))?;
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

    impl IssuedNativeSelectionCompletion {
        /// Inspect actual issued owners; poisoned copies below enter only
        /// collection refusal checks and never an inventory or compiler input.
        pub(crate) fn assert_collection_history(owners: &[Arc<Self>]) {
            fn slow_append(
                retained: &[Arc<IssuedNativeSelectionCompletion>],
                incoming: &[Arc<IssuedNativeSelectionCompletion>],
            ) -> Result<Vec<Arc<IssuedNativeSelectionCompletion>>, CompileError> {
                let mut result = retained.to_vec();
                for completion in incoming {
                    if result.iter().any(|prior| prior == completion) {
                        continue;
                    }
                    for prior in &result {
                        for (binding, row) in &completion.rows {
                            if prior.rows.get(binding).is_some_and(|old| old != row) {
                                return Err(failure("slow exact HOME conflict"));
                            }
                        }
                    }
                    if result.iter().any(|prior| {
                        prior.identity == completion.identity
                            && prior.recorded != completion.recorded
                    }) {
                        return Err(failure("slow commitment collision"));
                    }
                    result.push(Arc::clone(completion));
                }
                Ok(result)
            }
            assert!(
                !owners.is_empty(),
                "the production owner census is nonempty"
            );
            for reverse in [false, true] {
                let mut order = owners.to_vec();
                if reverse {
                    order.reverse();
                }
                let mut indexed = NativeSelectionCompletions::default();
                let mut reference = Vec::new();
                for completion in &order {
                    indexed = indexed
                        .append(&NativeSelectionCompletions::one(Arc::clone(completion)))
                        .unwrap();
                    reference = slow_append(&reference, &[Arc::clone(completion)]).unwrap();
                    assert_eq!(indexed.owners.len(), reference.len());
                    assert!(reference
                        .iter()
                        .all(|owner| indexed.iter().any(|actual| actual == owner)));
                    let repeated = indexed.append(&indexed).unwrap();
                    assert!(repeated.owners.ptr_eq(&indexed.owners));
                    assert!(repeated.rows.ptr_eq(&indexed.rows));
                    assert_eq!(indexed, repeated);
                }
                let replay = order.iter().rev().fold(
                    NativeSelectionCompletions::default(),
                    |prior, owner| {
                        prior
                            .append(&NativeSelectionCompletions::one(Arc::clone(owner)))
                            .unwrap()
                    },
                );
                assert_eq!(indexed, replay);
                assert_eq!(indexed.recorded, replay.recorded);
            }
            let actual = &owners[0];
            let copy = || Self {
                producer_sha256: actual.producer_sha256,
                profile_sha256: actual.profile_sha256,
                artifacts: actual.artifacts.clone(),
                rows: actual.rows.clone(),
                packages: Arc::clone(&actual.packages),
                retained: Arc::clone(&actual.retained),
                recorded: Arc::clone(&actual.recorded),
                identity: actual.identity,
            };
            let original = NativeSelectionCompletions::one(Arc::clone(actual));
            let mut collision = copy();
            Arc::make_mut(&mut collision.recorded).profile_sha256[0] ^= 1;
            assert!(original
                .append(&NativeSelectionCompletions::one(Arc::new(collision)))
                .is_err());
            let mut inconsistent = copy();
            inconsistent.producer_sha256[0] ^= 1;
            assert!(original
                .append(&NativeSelectionCompletions::one(Arc::new(inconsistent)))
                .is_err());
            assert!(Arc::ptr_eq(&original.owners[&actual.identity], actual));
        }
    }

    #[test]
    fn native_completion_row_index_preserves_exact_scopes_and_ordered_slots() {
        use tidepool_repr::execution_schema::ValueId;
        let binding = |scope| NativeGroupBinding {
            binding: NativeGroupIssuer {
                artifact: ArtifactId([31; 32]),
                selection: ArtifactSelectionId([scope; 32]),
            },
            original_ordinal: 7,
        };
        let row = |values: &[u32]| {
            Arc::new(NativeCompletionRowSnapshotFacts {
                imports: values
                    .iter()
                    .map(|value| NativeCompletionImportSnapshotFacts::Source {
                        binding: binding(1),
                        value: ValueId(*value),
                    })
                    .collect::<Vec<_>>()
                    .into(),
            })
        };
        // These are scalar facts only; no acquired completion is issued.
        let first = imbl::OrdMap::unit(binding(1), row(&[1, 2]));
        let second = imbl::OrdMap::unit(binding(2), row(&[2, 1]));
        let both = append_native_completion_rows(&first, &second).unwrap();
        assert_eq!(
            both.len(),
            2,
            "equal payloads retain their distinct full scopes"
        );
        assert!(Arc::ptr_eq(&both[&binding(1)], &first[&binding(1)]));
        assert!(Arc::ptr_eq(&both[&binding(2)], &second[&binding(2)]));
        let conflicting = imbl::OrdMap::unit(binding(1), row(&[2, 1]));
        assert!(append_native_completion_rows(&both, &conflicting).is_err());
        assert_eq!(both[&binding(1)].imports, first[&binding(1)].imports);
        assert_eq!(append_native_completion_rows(&both, &first).unwrap(), both);
    }

    proptest::proptest! {
        #[test]
        fn native_completion_row_index_matches_pairwise_history(
            history in proptest::collection::vec(
                proptest::collection::vec((0u8..4, 0u8..4, 0u8..4), 0..8), 0..12
            )
        ) {
            use tidepool_repr::execution_schema::ValueId;
            // Scalar data tests conflict indexing, never issued completion authority.
            let mut prior: Vec<imbl::OrdMap<NativeGroupBinding, Arc<NativeCompletionRowSnapshotFacts>>> = Vec::new();
            let mut indexed = imbl::OrdMap::new();
            for entries in history {
                let incoming = entries.into_iter().map(|(scope, ordinal, value)| {
                    let binding = NativeGroupBinding {
                        binding: NativeGroupIssuer {
                            artifact: ArtifactId([31; 32]),
                            selection: ArtifactSelectionId([scope; 32]),
                        },
                        original_ordinal: ordinal as u32,
                    };
                    (binding, Arc::new(NativeCompletionRowSnapshotFacts {
                        imports: vec![NativeCompletionImportSnapshotFacts::Source {
                            binding, value: ValueId(value as u32),
                        }].into(),
                    }))
                }).collect::<imbl::OrdMap<_, _>>();
                let conflict = prior.iter().any(|old| incoming.iter().any(|(key, row)| {
                    old.get(key).is_some_and(|previous| previous != row)
                }));
                let actual = append_native_completion_rows(&indexed, &incoming);
                proptest::prop_assert_eq!(actual.is_err(), conflict);
                if let Ok(merged) = actual {
                    prior.push(incoming);
                    let recomputed = prior.iter().flat_map(|rows| rows.iter())
                        .map(|(key, row)| (*key, Arc::clone(row)))
                        .collect::<BTreeMap<_, _>>();
                    proptest::prop_assert_eq!(merged.iter().collect::<Vec<_>>(), recomputed.iter().collect::<Vec<_>>());
                    for (key, row) in &indexed {
                        proptest::prop_assert!(Arc::ptr_eq(row, &merged[key]));
                    }
                    indexed = merged;
                }
            }
        }
    }

    #[test]
    fn native_completion_fact_wire_keeps_full_rows_and_rejects_noncanonical_keys() {
        use tidepool_repr::execution_schema::{SymbolIdentity, ValueId};
        let group = |ordinal| NativeGroupBinding {
            binding: NativeGroupIssuer {
                artifact: ArtifactId([31; 32]),
                selection: ArtifactSelectionId([41; 32]),
            },
            original_ordinal: ordinal,
        };
        let package = ArtifactBinding {
            artifact: ArtifactId([51; 32]),
            selection: ArtifactSelectionId([61; 32]),
        };
        // Pure wire data never enters a completion issuer in this control.
        let facts = NativeCompletionSnapshotFacts {
            version: 1,
            producer_sha256: [71; 32],
            profile_sha256: [81; 32],
            rows: [
                (
                    group(7),
                    Arc::new(NativeCompletionRowSnapshotFacts {
                        imports: vec![
                            NativeCompletionImportSnapshotFacts::Source {
                                binding: group(11),
                                value: ValueId(4),
                            },
                            NativeCompletionImportSnapshotFacts::Package {
                                binding: package,
                                value: ValueId(5),
                            },
                            NativeCompletionImportSnapshotFacts::Package {
                                binding: package,
                                value: ValueId(5),
                            },
                        ]
                        .into(),
                    }),
                ),
                (
                    group(11),
                    Arc::new(NativeCompletionRowSnapshotFacts {
                        imports: vec![NativeCompletionImportSnapshotFacts::Retained {
                            binding: package,
                            identity: SymbolIdentity {
                                unit: "fixture".into(),
                                module: "Value".into(),
                                namespace: "value".into(),
                                occurrence: "captured".into(),
                                record_parent: Some("Record".into()),
                            },
                            generation: 3,
                        }]
                        .into(),
                    }),
                ),
            ]
            .into_iter()
            .collect(),
            package_roots: vec![package, package].into(),
        };
        let encoded = fvm_ipld_encoding::to_vec(&facts).unwrap();
        let decoded: NativeCompletionSnapshotFacts =
            fvm_ipld_encoding::from_slice(&encoded).unwrap();
        assert_eq!(decoded, facts);
        assert_eq!(decoded.identity().unwrap(), facts.identity().unwrap());
        for duplicate in [false, true] {
            let mut wire: ciborium::value::Value =
                ciborium::de::from_reader(encoded.as_slice()).unwrap();
            let ciborium::value::Value::Map(fields) = &mut wire else {
                panic!("fact object")
            };
            let (_, rows) = fields
                .iter_mut()
                .find(|(key, _)| key.as_text() == Some("rows"))
                .unwrap();
            let ciborium::value::Value::Array(rows) = rows else {
                panic!("row sequence")
            };
            if duplicate {
                rows.push(rows[0].clone());
            } else {
                rows.reverse();
            }
            let mut malformed = Vec::new();
            ciborium::ser::into_writer(&wire, &mut malformed).unwrap();
            assert!(
                fvm_ipld_encoding::from_slice::<NativeCompletionSnapshotFacts>(&malformed).is_err()
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
        assert_ne!(
            row.original.as_ref().unwrap().0,
            foreign.original.as_ref().unwrap().0
        );
        let primary = first.compiler_primary_input(owner).unwrap().unwrap();
        assert_eq!(primary.original(), Some(row.original.as_ref().unwrap().0));
        assert!(Arc::ptr_eq(
            primary.entry(),
            &row.original.as_ref().unwrap().1
        ));
        assert!(first.issue_compiler_primary_input(owner, foreign).is_err());
        assert!(second.issue_compiler_primary_input(owner, row).is_err());
        let mut wrong_canonical = row.clone();
        wrong_canonical.interface = first.selected[&entries[2].descriptor.owner]
            .interface
            .clone();
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
                .capture_world_primary_compiler_inputs([2; 32])
                .unwrap();
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
                    &support.selected[owner].interface.1,
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
            let original = context
                .capture_world_primary_compiler_inputs([2; 32])
                .unwrap();
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
            let changed = original.with_type_interfaces(&[incoming]).unwrap();
            let artifacts = changed.inputs.artifacts();
            assert!(artifacts.reads.read_projection.get().is_none());
            let observed_inventory = artifacts.inventory();
            let before = observed_inventory.metrics();
            let empty_budget = InventoryOperation::new(InventoryDecodeLimits {
                max_work: 0,
                ..Default::default()
            });
            let empty = changed
                .inputs
                .demanded_compiler_projection(&BTreeSet::new(), &empty_budget)
                .unwrap();
            assert!(empty.owners().is_empty());
            assert_eq!(empty.work(), DemandCompilerProjectionWork::default());
            let empty_native = changed
                .inputs
                .demanded_native_inputs(empty, &BTreeSet::new(), &empty_budget)
                .unwrap();
            assert!(empty_native
                .native_availability()
                .persistent_facts()
                .is_empty());
            assert_eq!(empty_budget.work_usage().unwrap(), (0, 0));
            let operation = InventoryOperation::new(Default::default());
            let demanded = changed
                .inputs
                .demanded_compiler_projection(&BTreeSet::from([owner.clone()]), &operation)
                .unwrap();
            assert_eq!(demanded.owners(), &BTreeSet::from([owner]));
            assert_eq!(demanded.work().selected_rows, 1);
            assert_eq!(demanded.work().payload_lookups, 1);
            assert!(Arc::ptr_eq(
                &demanded.metadata().retained,
                &changed.inputs.metadata().retained
            ));
            let selected = changed
                .inputs
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
            let maintained = &changed.inputs.metadata().retained;
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
                crate::declaration_context::ExactDeclarationContext::from_authenticated_execution(
                    [2; 32],
                    &view,
                    projection,
                    lexical.into_values().collect(),
                    root.clone(),
                    &[],
                )
                .unwrap();
            let inputs = context
                .capture_world_primary_compiler_inputs([2; 32])
                .unwrap();
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
                    proptest::prop_assert_eq!(after.interface.0, before.interface.0);
                    proptest::prop_assert!(Arc::ptr_eq(&after.interface.1, &before.interface.1));
                    proptest::prop_assert_eq!(&after.original, &before.original);
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
        let full_offers = IssuedNativeOffer::issue_context_offers(&projection, &full, &[]).unwrap();
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
            let offers =
                IssuedNativeOffer::issue_context_offers(&projection, &chosen, &[]).unwrap();
            let availability =
                crate::declaration_context::RequestNativeAvailability::issue_checked(
                    &projection,
                    &chosen,
                    &offers,
                )
                .unwrap();
            let source =
                CompilerSourceSupport::new(projection.clone(), chosen, offers, availability)
                    .unwrap()
                    .retain_offers(full_offers.iter().cloned())
                    .unwrap();
            let before = source.artifacts.capture_graph_selection();
            let old_edges = source.artifacts.binding_dependencies();
            let required_binding = source.selected[&entry.descriptor.owner].interface.0;
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
            assert_eq!(actual.native_availability, source.native_availability);
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
            assert_eq!(actual.native_availability, oracle.native_availability);
            assert_eq!(
                graph.native_groups,
                oracle.artifacts.capture_graph_selection().native_groups
            );
            let repeated = actual.admit_value_interfaces(&[value]).unwrap();
            assert!(Arc::ptr_eq(
                &actual.artifacts.lease,
                &repeated.artifacts.lease
            ));
            assert_eq!(actual.selected, repeated.selected);
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
            let a = first.selected[&canonical.descriptor.owner].interface.0;
            let b = second.selected[&canonical.descriptor.owner].interface.0;
            proptest::prop_assert_eq!(a.artifact, b.artifact);
            proptest::prop_assert_ne!(a, b);
            let mixed = second.merge(&first).unwrap();
            let old_value = mixed.selected[&ExactModuleIdentity { unit: "unit".into(), module: "IssuedValue".into() }].interface.0;
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
            proptest::prop_assert_eq!(appended.selected[&canonical.descriptor.owner].interface.0, b);
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
            let source = crate::declaration_context::ValidatedOriginalSource::inputs_from(&context);
            let signature = type_signature("unit", "Selected");
            type_query_work::take();
            let mut narrowed = Vec::new();
            for _ in 0..queries {
                narrowed.push(source.project_type_interfaces(&[], &[&signature]).unwrap());
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
                proptest::prop_assert_eq!(&actual.selected, &oracle.selected);
                proptest::prop_assert_eq!(actual.artifacts.selection_snapshot(), oracle.artifacts.selection_snapshot());
                proptest::prop_assert_eq!(actual.selected.home_units.iter().cloned().collect::<BTreeSet<_>>(), BTreeSet::from(["unit".to_owned()]));
                for (owner, original) in &expected {
                    proptest::prop_assert_eq!(&actual.selected[owner].interface.1, original);
                }
                proptest::prop_assert!(result.artifact_view().selected_native_groups().is_empty());
                proptest::prop_assert!(result.lexical_graph().is_empty());
            }
            // The same Name is a missing home in the full owner, but an external
            // unit in the restricted owner. Its classification must not leak.
            if unrelated > 0 {
                let absent = type_signature("other_unit_0", "Absent");
                proptest::prop_assert!(context.select_original_type_interfaces(&[], &[&absent]).is_err());
                let external = narrowed[0].select_original_type_interfaces(&[], &[&absent]).unwrap();
                proptest::prop_assert!(external.compiler_input_roles().is_empty());
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
                assert_eq!(row.interface.0, *binding);
                let ArtifactPayload::Canonical(interface) = &row.interface.1.payload else {
                    panic!("canonical narrowed row")
                };
                assert_eq!(interface.interface_bytes(), bytes.as_slice());
                assert!(row.original.is_none());
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
            assert_eq!(source.selected, recomputed.selected);
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
                    .interface
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
            let offers =
                IssuedNativeOffer::issue_context_offers(&historical_projection, &view, &[])
                    .unwrap();
            assert!(!offers.is_empty());
            let support =
                CompilerSourceSupport::new(projection, view, offers, Default::default()).unwrap();
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
            let offers = reusable
                .native_offers
                .iter()
                .filter_map(
                    |offer| match offer.is_relevant_to_support(&legacy.projection) {
                        Ok(true) => Some(Ok(offer.clone())),
                        Ok(false) => None,
                        Err(error) => Some(Err(error)),
                    },
                )
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            let legacy = reusable
                .rebuild_narrowed(legacy.projection, legacy.artifacts, offers)
                .unwrap();
            assert_eq!(narrowed.selected, legacy.selected);
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
            let expected_binding = legacy.selected[&historical.descriptor.owner].interface.0;
            drop(legacy);
            drop(reusable);
            // Retained semantic payload/canonical pairing; this fixture does
            // not establish last physical materialization release.
            let row = &narrowed.selected[&historical.descriptor.owner];
            assert_eq!(
                row.original.as_ref().unwrap().1.descriptor.id,
                historical.descriptor.id
            );
            assert_eq!(row.interface.0, expected_binding);

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
        assert_eq!(source.native_availability, reversed.native_availability);
        assert_eq!(source.selected, reversed.selected);
        assert_eq!(
            source.input_decisions.recorded_facts(),
            reversed.input_decisions.recorded_facts()
        );
        assert_eq!(source.native_fact_owner, reversed.native_fact_owner);
        crate::exact_context::take_native_fact_work();
        assert_eq!(source, reversed);
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
            for (binding, entry) in std::iter::once(&row.interface).chain(row.original.iter()) {
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
                input_decisions: rebuilt.input_decisions.facts.clone(),
            },
        )
        .unwrap();
        assert_eq!(
            maintained.commitment(),
            rebuilt.native_fact_owner().commitment()
        );
        // Repeated identical enrichment reuses the exact closed state instead
        // of re-merging custody or reminting the fact owner.
        let repeated = rebuilt.with_native_completions_from(&rebuilt).unwrap();
        assert!(Arc::ptr_eq(
            repeated.native_fact_owner(),
            rebuilt.native_fact_owner()
        ));
        assert_eq!(repeated.native_completions(), rebuilt.native_completions());
    }

    #[test]
    fn source_support_preserves_completion_custody_after_role_narrowing() {
        let inventory = ArtifactInventory::default();
        let selected = Arc::new(crate::artifact_inventory::tests::entry("SourceRoot", &[]));
        let completion_only = Arc::new(crate::artifact_inventory::tests::entry(
            "CompletionOnly",
            &[],
        ));
        let full = inventory
            .admit_shared(
                &inventory.empty_view(),
                vec![Arc::clone(&selected), Arc::clone(&completion_only)],
            )
            .unwrap();
        let projection =
            CompilerInputProjection::from_issued_entries(&[Arc::clone(&selected)]).unwrap();
        let support =
            CompilerSourceSupport::from_selected_issuance(projection, full.clone()).unwrap();

        let operation =
            tidepool_repr::execution_schema::InventoryOperation::new(Default::default());
        let completion_view = full
            .select_roots(vec![completion_only.descriptor.id])
            .unwrap();
        let packages = super::super::package_code::IssuedPackageCodeGraph::issue(
            &completion_view,
            Vec::new(),
            Vec::new(),
            &[],
            &operation,
        )
        .unwrap();
        let completion = IssuedNativeSelectionCompletion::issue_parts(
            [7; 32],
            [9; 32],
            completion_view,
            Default::default(),
            packages,
            Arc::from([]),
            &operation,
        )
        .unwrap();
        let mut draft = support.reopen();
        draft
            .append_completion_refs(&NativeSelectionCompletions::one(Arc::clone(&completion)))
            .unwrap();
        let support = draft.close().unwrap();

        let narrowed = support
            .source_support(&BTreeSet::from([selected.descriptor.owner.clone()]))
            .unwrap();
        assert!(narrowed
            .artifacts
            .artifact_ids()
            .contains(&completion_only.descriptor.id));
        assert_eq!(narrowed.native_completions().iter().count(), 1);
        assert!(narrowed
            .native_completions()
            .iter()
            .any(|actual| Arc::ptr_eq(actual, &completion)));
        assert!(!narrowed
            .projection()
            .snapshot_roles()
            .contains_key(&completion_only.descriptor.owner));
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
                let captured = PublicationNativeAuthority::capture(&request).unwrap();
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
            assert_eq!(changed.native_availability, expected.native_availability);
            assert_eq!(
                changed.input_decisions.facts,
                expected.input_decisions.facts
            );
            assert_eq!(source.projection, prior_projection);
            assert_eq!(source.artifacts.capture_graph_selection(), prior_view);
            for (owner, prior) in source.selected.iter() {
                assert!(Arc::ptr_eq(
                    &changed.selected.rows[owner].interface.1,
                    &prior.interface.1
                ));
                if let Some((_, original)) = &prior.original {
                    assert!(Arc::ptr_eq(
                        &changed.selected.rows[owner].original.as_ref().unwrap().1,
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
                changed.native_fact_owner().input_decisions,
                source.native_fact_owner().input_decisions
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
    fn program_fragment_pairs_native_rows_with_final_payload_acquisitions() {
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
                assert_eq!(receiving, incoming);
                assert!(!Arc::ptr_eq(receiving, incoming));
                let (ArtifactPayload::Original(first), ArtifactPayload::Original(second)) =
                    (&receiving.payload, &incoming.payload)
                else {
                    unreachable!()
                };
                assert_eq!(first.source_sha256(), second.source_sha256());
                assert_eq!(
                    Arc::ptr_eq(
                        first.original_native().unwrap(),
                        second.original_native().unwrap()
                    ),
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
                assert_eq!(row.interface.0, other.interface.0);
                assert_eq!(
                    row.original.as_ref().unwrap().0,
                    other.original.as_ref().unwrap().0
                );
                assert!(!Arc::ptr_eq(
                    &row.original.as_ref().unwrap().1,
                    &other.original.as_ref().unwrap().1,
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
                        assert_eq!(prior, contribution);
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
            if distinct_witness {
                assert!(
                    matches!(appended, Err(CompileError::ArtifactInventory(error))
                    if matches!(error.failure, ArtifactInventoryFailure::MetadataConflict { artifact }
                        if receiving_entries.iter().any(|entry| entry.descriptor.id == artifact)))
                );
                continue;
            }
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
            for entry in metadata
                .entries
                .values()
                .chain(metadata.retained.artifacts.values())
                .chain(metadata.retained.entries.values())
                .chain(metadata.retained.interface_representatives.values())
            {
                assert!(Arc::ptr_eq(
                    entry,
                    &support.artifacts.lease.payloads[&entry.descriptor.id]
                ));
            }
            for row in support.selected.values() {
                for (binding, entry) in std::iter::once(&row.interface).chain(row.original.iter()) {
                    assert!(Arc::ptr_eq(
                        entry,
                        &support.artifacts.lease.payloads[&binding.artifact]
                    ));
                }
            }
            for row in support.native_availability.rows() {
                let id = row.original_artifact_id();
                let payload = &support.artifacts.lease.payloads[&id];
                assert!(Arc::ptr_eq(
                    row.target_original(row.original_owner()).unwrap(),
                    payload
                ));
                assert!(Arc::ptr_eq(payload, &metadata.retained.artifacts[&id]));
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
                    &closed.artifacts.lease.payloads[&id],
                ));
                assert!(Arc::ptr_eq(
                    row,
                    support.native_availability.issued_row(id).unwrap()
                ));
            }
            let unchanged = closed
                .native_availability
                .retain_acquisitions(&closed.artifacts)
                .unwrap();
            assert!(unchanged.same_acquisition(&closed.native_availability));
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
            assert_eq!(source.native_availability, changed.native_availability);
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
                assert_eq!(&changed.selected[owner], row);
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
            assert_eq!(changed.selected, oracle.selected);
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
            source.selected[&owner].interface.0,
            same_bytes.selected[&owner].interface.0
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
            let original =
                crate::declaration_context::ValidatedOriginalSource::inputs_from(&execution);
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
            let projected_context = original
                .project_type_interfaces(&[], &[&signature])
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
            assert!(Arc::ptr_eq(&original.execution_context(), &execution));
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
            assert_eq!(joined.native_availability, support.native_availability);
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
    fn selected_native_acquisitions_follow_choices_without_promoting_availability() {
        let entries = decision_affinity_entries();
        let inventory = ArtifactInventory::default();
        let view = inventory
            .admit_shared(&inventory.empty_view(), entries[..2].to_vec())
            .unwrap();
        let projection = CompilerInputProjection::from_issued_entries(&entries[..2]).unwrap();
        let a = CompilerSourceSupport::from_selected_issuance(projection.clone(), view.clone())
            .unwrap();
        let b = CompilerSourceSupport::from_selected_issuance(projection, view).unwrap();
        assert_eq!(
            a, b,
            "compiler graph equality remains independent of live acquisition"
        );
        let owner = &entries[1].descriptor.owner;
        let group = a.input_decisions.facts[owner].groups[&7];
        let issuer_a = a.input_decisions.selected[&group].acquisitions()[0].clone();
        let issuer_b = b.input_decisions.selected[&group].acquisitions()[0].clone();
        assert!(!issuer_a.same_issuance(&issuer_b));
        assert_eq!(
            issuer_a, issuer_b,
            "equal graph facts do not identify a live acquisition"
        );
        let assert_sources =
            |support: &CompilerSourceSupport, expected: &[&IssuedNativeOffer], missing: bool| {
                let selected = &support.input_decisions.selected[&group];
                assert_eq!(
                    matches!(selected, SelectedNativeSupport::Missing(_)),
                    missing
                );
                assert_eq!(selected.acquisitions().len(), expected.len());
                for expected in expected {
                    assert!(selected
                        .acquisitions()
                        .iter()
                        .any(|actual| actual.same_issuance(expected)));
                }
            };
        let available_b = a
            .clone()
            .retain_normalized_availability(b.native_availability())
            .unwrap()
            .retain_offers(b.native_offers.iter().cloned())
            .unwrap();
        assert_sources(&available_b, &[&issuer_a], false);
        assert_eq!(
            available_b.artifacts.capture_graph_selection(),
            a.artifacts.capture_graph_selection()
        );
        for (left, right) in [(&a, &b), (&b, &a)] {
            let both = left.merge(right).unwrap();
            assert_sources(&both, &[&issuer_a, &issuer_b], false);
            assert_eq!(
                both.input_decisions.recorded_facts(),
                a.input_decisions.recorded_facts()
            );
            assert_eq!(
                both.artifacts.capture_graph_selection(),
                a.artifacts.capture_graph_selection()
            );
            let narrowed = both
                .source_support(&BTreeSet::from([owner.clone()]))
                .unwrap();
            assert_sources(&narrowed, &[&issuer_a, &issuer_b], false);
            let expected_roots = narrowed
                .input_decisions
                .facts
                .values()
                .flat_map(|fact| fact.groups.values().copied())
                .collect::<BTreeSet<_>>();
            assert_eq!(
                narrowed
                    .input_decisions
                    .selected
                    .keys()
                    .copied()
                    .collect::<BTreeSet<_>>(),
                expected_roots
            );
        }
        let facts = a.native_fact_owner();
        let recovered = a
            .restore_native_maps(
                &a.projection.recorded_roles,
                &facts.offers,
                &facts.native_availability,
                &facts.input_decisions,
            )
            .unwrap();
        assert_sources(&recovered, &[], true);
        assert_eq!(
            recovered.input_decisions.recorded_facts(),
            a.input_decisions.recorded_facts()
        );
        for (left, right) in [(&a, &recovered), (&recovered, &a)] {
            assert_sources(&left.merge(right).unwrap(), &[&issuer_a], true);
        }
        // This is the former sparse-overwrite state: selected facts exist,
        // but no live acquisition was observed. It cannot be upgraded by A.
        let sparse = IssuedInputNativeDecisions {
            facts: a.input_decisions.facts.clone(),
            selected: Default::default(),
        };
        for (left, right) in [(&sparse, &a.input_decisions), (&a.input_decisions, &sparse)] {
            let mut merged = left.clone();
            merged.merge_selected(right).unwrap();
            let SelectedNativeSupport::Missing(acquired) = &merged.selected[&group] else {
                panic!("selected missing evidence must remain missing");
            };
            assert_eq!(acquired.len(), 1);
            assert!(acquired[0].same_issuance(&issuer_a));
        }
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
                (row.interface.0, row.original.as_ref().unwrap().0)
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
            assert_eq!(joined.selected, prior_support.selected);
            assert_eq!(
                joined.native_offer_observations().unwrap(),
                prior_support.native_offer_observations().unwrap()
            );
            for row in joined.selected.values() {
                for (binding, entry) in std::iter::once(&row.interface).chain(row.original.iter()) {
                    assert!(
                        Arc::ptr_eq(entry, &joined.artifacts.lease.payloads[&binding.artifact]),
                        "chosen row acquires the final payload owner"
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
            assert_eq!(supports[0].selected[owner], supports[1].selected[owner]);
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
            let finalized = crate::certified_products::finalize_original_for_inventory(
                product.clone(),
                &mut crate::recovery_artifacts::PackageInterfaceValidation::default(),
            )
            .unwrap();
            let definitions =
                crate::certified_products::original_native_group_definitions(&finalized, ordinal)
                    .unwrap();
            let binder = match &definitions.bindings()[0] {
                tidepool_repr::execution_schema::Group::NonRecursive(binding) => &binding.identity,
                tidepool_repr::execution_schema::Group::Recursive(bindings) => {
                    &bindings[0].identity
                }
            }
            .clone();
            PendingImportOwner::Source {
                owner: finalized.owner().clone(),
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
                assert_eq!(offers, offers_oracle);
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
                    for row in retained.selected.values() {
                        for (binding, entry) in
                            std::iter::once(&row.interface).chain(row.original.iter())
                        {
                            assert!(Arc::ptr_eq(
                                entry,
                                &retained.artifacts.lease.payloads[&binding.artifact]
                            ));
                        }
                    }
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
            assert_eq!(finalized, expected);
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
                assert_eq!(repeated, expected);
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
            assert_eq!(narrowed, expected);
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
