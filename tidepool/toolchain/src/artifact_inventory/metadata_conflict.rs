//! Typed observations of a rejected immutable-fact comparison. These never
//! participate in equality, selection or permission decisions.
use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ArtifactMetadataField {
    Descriptor,
    Requirements,
    InterfaceSeals,
    FactKind,
    OriginalOwner,
    OriginalProduct,
    OriginalCertificate,
    OriginalExecutionSource,
    CanonicalProducer,
    CanonicalCertificate,
    CanonicalInterface,
    CanonicalPackages,
    CanonicalCore,
    CanonicalOrigin,
    CanonicalHomeUnits,
    JoinedInterface,
    JoinedRole,
    JoinedRequirements,
    PackageProduct,
}

pub(super) fn refusal(
    existing: &Arc<ArtifactEntry>,
    incoming: &Arc<ArtifactEntry>,
) -> CompileError {
    let mut fields = Vec::new();
    for (different, field) in [
        (
            existing.descriptor != incoming.descriptor,
            ArtifactMetadataField::Descriptor,
        ),
        (
            existing.requirements != incoming.requirements,
            ArtifactMetadataField::Requirements,
        ),
        (
            existing.interface_seals != incoming.interface_seals,
            ArtifactMetadataField::InterfaceSeals,
        ),
    ] {
        if different {
            fields.push(field);
        }
    }
    match (&existing.facts, &incoming.facts) {
        (ArtifactFactsHandle::Original(a), ArtifactFactsHandle::Original(b)) => {
            a.record_metadata_differences(b, &mut fields)
        }
        (ArtifactFactsHandle::Canonical(a), ArtifactFactsHandle::Canonical(b)) => {
            a.record_metadata_differences(b, &mut fields)
        }
        (ArtifactFactsHandle::Joined(a), ArtifactFactsHandle::Joined(b)) => {
            for (different, field) in [
                (
                    a.interface != b.interface,
                    ArtifactMetadataField::JoinedInterface,
                ),
                (a.role != b.role, ArtifactMetadataField::JoinedRole),
                (
                    a.requirements != b.requirements,
                    ArtifactMetadataField::JoinedRequirements,
                ),
            ] {
                if different {
                    fields.push(field);
                }
            }
        }
        (ArtifactFactsHandle::PackageCode(a), ArtifactFactsHandle::PackageCode(b)) => {
            if a != b {
                fields.push(ArtifactMetadataField::PackageProduct);
            }
        }
        _ => fields.push(ArtifactMetadataField::FactKind),
    }
    ArtifactInventoryError {
        failure: ArtifactInventoryFailure::MetadataConflict {
            artifact: incoming.descriptor.id,
        },
        diagnostic_artifacts: None,
        native_refusal: None,
        owner_conflict: Some(ArtifactOwnerConflictEvidence::Metadata {
            existing: Arc::clone(existing),
            incoming: Arc::clone(incoming),
            fields,
            selected: None,
        }),
    }
    .into()
}
