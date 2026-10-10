//! Durable observations of the exact dependency selection issued for an entry.

use super::*;
use crate::artifact_inventory::{
    ArtifactEntry, ArtifactId, CompilerInputRole, ExactArtifactSelection,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LinkedOriginal {
    unit: String,
    module: String,
    module_version: [u8; 32],
    interface_sha256: [u8; 32],
    product_sha256: [u8; 32],
}

impl LinkedOriginal {
    fn from_owner(owner: &CachedHomeOwner) -> Self {
        Self {
            unit: owner.unit.clone(),
            module: owner.module.clone(),
            module_version: owner.module_version.0,
            interface_sha256: owner.skinny_iface_sha256,
            product_sha256: owner.product_sha256,
        }
    }
}

/// Decoding preserves observations only. Reopening reissues the selection from
/// authenticated compiler receipts and requires equality with these facts.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EntryDependencySelection {
    linked: Vec<LinkedOriginal>,
    compiler_roles: Vec<CompilerInputRole>,
    native_closure: ExactArtifactSelection,
}

impl CertifiedSourceSelection {
    pub(crate) fn entry_dependency_selection(
        &self,
        view: &crate::artifact_inventory::ArtifactView,
        candidates: &CandidateSet,
    ) -> CertResult<EntryDependencySelection> {
        let closure = self.selected_original_closure(view)?;
        let projection = self.compiler_projection(view)?;
        let interfaces = self
            .visible_modules()
            .map(|(_, row)| row.interface())
            .collect::<BTreeSet<ArtifactId>>();
        let originals = closure
            .native_closure
            .entries()
            .into_iter()
            .filter_map(|entry| match &entry.payload {
                crate::artifact_inventory::ArtifactPayload::Original(product) => {
                    Some(product.owner().clone())
                }
                _ => None,
            })
            .collect::<BTreeSet<_>>();
        let linked = candidates
            .by_owner
            .values()
            .filter(|candidate| {
                originals.contains(&candidate.owner)
                    || interfaces.contains(
                        &ArtifactEntry::canonical(candidate.original_module_interface.clone())
                            .descriptor
                            .id,
                    )
            })
            .map(|candidate| LinkedOriginal::from_owner(&candidate.owner))
            .collect();
        Ok(EntryDependencySelection {
            linked,
            compiler_roles: projection.roles(),
            native_closure: ExactArtifactSelection::capture(&closure.native_closure),
        })
    }
}

impl EntryDependencySelection {
    pub(crate) fn has_linked_originals(&self) -> bool {
        !self.linked.is_empty()
    }

    /// Retain only the original identities persisted by the compilation issuer.
    /// Offered inventory membership alone cannot choose an entry's dependencies.
    pub(crate) fn restrict_candidates(
        &self,
        mut candidates: CandidateSet,
    ) -> Result<CandidateSet, crate::CompileError> {
        let mut selected = BTreeSet::new();
        for original in &self.linked {
            let key = (original.unit.clone(), original.module.clone());
            let candidate = candidates.by_owner.get(&key).ok_or_else(|| {
                crate::CompileError::ExtractFailed("linked entry original is unavailable".into())
            })?;
            if LinkedOriginal::from_owner(&candidate.owner) != *original || !selected.insert(key) {
                return Err(crate::CompileError::ExtractFailed(
                    "linked entry original identity differs or repeats".into(),
                ));
            }
        }
        candidates.by_owner.retain(|key, _| selected.contains(key));
        candidates.native_availability.retain(|product| {
            self.linked
                .iter()
                .any(|original| LinkedOriginal::from_owner(product.owner()) == *original)
        });
        Ok(candidates)
    }
}
