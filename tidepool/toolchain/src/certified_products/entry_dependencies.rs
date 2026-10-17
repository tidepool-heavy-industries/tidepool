//! Durable observations of the exact dependency selection issued for an entry.

use super::*;
use crate::artifact_inventory::{
    ArtifactId, CompilerInputRole, ExactArtifactSelection, ExactArtifactSelectionDigest,
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

/// Original-entry receipts select interfaces and reusable originals. Published
/// declaration policy belongs to a runtime context, never an entry observation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum EntryCompilerRole {
    InterfaceOnly {
        interface: ArtifactId,
    },
    ReusableOriginal {
        interface: ArtifactId,
        original: ArtifactId,
    },
}

impl TryFrom<CompilerInputRole> for EntryCompilerRole {
    type Error = CertificationError;

    fn try_from(role: CompilerInputRole) -> CertResult<Self> {
        match role {
            CompilerInputRole::InterfaceOnly { interface } => Ok(Self::InterfaceOnly { interface }),
            CompilerInputRole::ReusableOriginal {
                interface,
                original,
            } => Ok(Self::ReusableOriginal {
                interface,
                original,
            }),
            CompilerInputRole::PublishedSourceOriginal { .. } => Err(CertificationError::Mismatch(
                "original entry cannot retain published declaration policy",
            )),
        }
    }
}

/// Decoding preserves observations only. Reopening reissues the selection from
/// authenticated compiler receipts and requires equality with these facts.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EntryDependencySelection {
    linked: Vec<LinkedOriginal>,
    compiler_roles: Vec<EntryCompilerRole>,
    native_closure: ExactArtifactSelectionDigest,
}

impl CertifiedSourceSelection {
    pub(crate) fn entry_dependency_selection(
        &self,
        view: &crate::artifact_inventory::ArtifactView,
        candidates: &CandidateSet,
    ) -> CertResult<EntryDependencySelection> {
        let closure = self.selected_original_closure(view)?;
        let projection = self.compiler_projection(view)?;
        let borrowed = self
            .native_selections()
            .filter(|selection| matches!(selection.version, ReceiptSourceVersion::Original))
            .map(|selection| selection.owner.clone())
            .collect::<BTreeSet<_>>();
        let linked = candidates
            .by_owner
            .values()
            .filter(|candidate| borrowed.contains(&candidate.owner))
            .map(|candidate| LinkedOriginal::from_owner(&candidate.owner))
            .collect();
        Ok(EntryDependencySelection {
            linked,
            compiler_roles: projection
                .roles()
                .into_iter()
                .map(EntryCompilerRole::try_from)
                .collect::<CertResult<_>>()?,
            native_closure: ExactArtifactSelection::capture(&closure.native_closure)
                .observation_digest()
                .map_err(|error| CertificationError::OriginalClosure(Box::new(error)))?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn property_config() -> ProptestConfig {
        let mut config = ProptestConfig::default();
        if std::env::var_os("PROPTEST_CASES").is_none() {
            config.cases = 64;
        }
        if let Some(path) = option_env!("TIDEPOOL_PROPTEST_REGRESSIONS") {
            config.failure_persistence = Some(Box::new(
                proptest::test_runner::FileFailurePersistence::Direct(path),
            ));
        }
        config
    }

    proptest! {
        #![proptest_config(property_config())]
        #[test]
        fn original_entry_roles_preserve_issued_observations(
            interface in any::<[u8; 32]>(),
            original in proptest::option::of(any::<[u8; 32]>()),
        ) {
            let interface = ArtifactId(interface);
            let issued = match original {
                Some(original) => CompilerInputRole::ReusableOriginal {
                    interface,
                    original: ArtifactId(original),
                },
                None => CompilerInputRole::InterfaceOnly { interface },
            };
            let previous_wire = serde_json::to_value(&issued).unwrap();
            let entry = EntryCompilerRole::try_from(issued).unwrap();
            let wire = serde_json::to_value(&entry).unwrap();
            prop_assert_eq!(&wire, &previous_wire);
            prop_assert_eq!(serde_json::from_value::<EntryCompilerRole>(wire).unwrap(), entry);
        }
    }

    #[test]
    fn original_entry_observations_refuse_runtime_context_policy() {
        // The original-entry producer cannot issue this variant. Refuse it at
        // decoding rather than importing a context graph from an observation.
        let inventory = crate::artifact_inventory::ArtifactInventory::default();
        let published = CompilerInputRole::PublishedSourceOriginal {
            interface: ArtifactId([7; 32]),
            original: ArtifactId([8; 32]),
            source_revision: "source revision".into(),
            original_input_identity: "input identity".into(),
            selection: ExactArtifactSelection::capture(&inventory.empty_view()),
            selection_sha256: [0; 32],
            native_offers: crate::artifact_inventory::PublishedNativeOfferReferences::capture(&[]).unwrap(),
        };
        let wire = serde_json::to_value(&published).unwrap();
        assert!(serde_json::from_value::<CompilerInputRole>(wire.clone()).is_ok());
        assert!(serde_json::from_value::<EntryCompilerRole>(wire).is_err());
        assert!(EntryCompilerRole::try_from(published).is_err());
        let extra_policy = serde_json::json!({
            "kind": "interface_only",
            "interface": ArtifactId([9; 32]),
            "selection": {},
        });
        assert!(serde_json::from_value::<EntryCompilerRole>(extra_policy).is_err());
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
