//! Read-only SOURCE metadata from an admitted primary and its exact acquired graph.

use std::collections::BTreeSet;
use std::sync::Arc;

use tidepool_repr::execution_schema::{InventoryOperation, SymbolIdentity};

use crate::artifact_inventory::{ArtifactBinding, ArtifactPayload, NativeGroupBinding};
use crate::certified_products::{
    original_native_group_definitions, original_native_group_imports,
    AuthenticatedOriginalBindings, CertificationError, ReceiptImportOwner,
};
use crate::declaration_join::ExactModuleIdentity;
use crate::CompileError;

use super::{NativeRetainedOccurrence, WorldPrimaryInputs};

/// An observation keeps the same admitted SOURCE context, exact input selection
/// and accounting owner alive. It cannot authorize native acquisition or values.
pub(crate) struct SourceDependencyBasis {
    _primary: WorldPrimaryInputs,
    _membership: Arc<AuthenticatedOriginalBindings>,
    rows: Vec<SourceDependencyRow>,
}

pub(crate) struct SourceDependencyRow {
    reference: ReceiptImportOwner,
    retained: Vec<OwnedRetainedOccurrence>,
}

#[derive(Eq, PartialEq, Ord, PartialOrd)]
struct OwnedRetainedOccurrence {
    demander: NativeGroupBinding,
    global_index: u32,
    binding: ArtifactBinding,
    identity: SymbolIdentity,
    generation: u64,
}

fn refuse(detail: &'static str) -> CompileError {
    CompileError::CompilerEvidence(Box::new(CertificationError::Mismatch(detail)))
}

impl SourceDependencyBasis {
    pub(crate) fn observe(
        primary: &WorldPrimaryInputs,
        references: Vec<ReceiptImportOwner>,
        operation: &Arc<InventoryOperation>,
    ) -> Result<Self, CompileError> {
        for reference in &references {
            crate::host_work::checkpoint()?;
            let ReceiptImportOwner::OriginalBinding { unit, module, .. } = reference else {
                return Err(refuse(
                    "SOURCE dependency query requires exact Original uses",
                ));
            };
            operation
                .charge(unit.len().saturating_add(module.len()))
                .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
            let owner = ExactModuleIdentity {
                unit: unit.clone(),
                module: module.clone(),
            };
            if !primary.context().source_scope().admits(&owner) {
                return Err(refuse(
                    "SOURCE dependency owner is outside admitted source scope",
                ));
            }
        }
        let membership = AuthenticatedOriginalBindings::issue(
            primary.inputs().shared_compiler_source_support(),
            Arc::clone(operation),
            references.iter(),
        )
        .map_err(|error| CompileError::CompilerEvidence(Box::new(error)))?;
        operation
            .reserve::<SourceDependencyRow>(references.len())
            .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
        let view = membership.acquisition().artifacts();
        let mut rows = Vec::with_capacity(references.len());
        for reference in references {
            crate::host_work::checkpoint()?;
            let ReceiptImportOwner::OriginalBinding { original, .. } = &reference else {
                return Err(refuse(
                    "authenticated SOURCE dependency reference changed kind",
                ));
            };
            let (_, ordinal) = membership
                .get(&reference)
                .ok_or_else(|| refuse("SOURCE dependency lacks authenticated pair membership"))?;
            let root = view.acquired_original_group_for_carrier(*original, *ordinal, operation)?;
            operation
                .reserve::<NativeGroupBinding>(1)
                .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
            let mut visited = BTreeSet::new();
            let mut pending = vec![root];
            let mut retained = Vec::new();
            while let Some(demander) = pending.pop() {
                crate::host_work::checkpoint()?;
                if visited.contains(&demander) {
                    continue;
                }
                operation
                    .reserve::<(NativeGroupBinding, [usize; 4])>(1)
                    .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
                visited.insert(demander);
                view.validate_acquired_original_group_bindings(
                    &BTreeSet::from([demander]),
                    operation,
                )?;
                let entry = view
                    .observed_payload(&demander.binding.artifact())
                    .ok_or_else(|| refuse("SOURCE dependency group lacks its acquired payload"))?;
                let ArtifactPayload::Original(product) = &entry.payload else {
                    return Err(refuse("SOURCE dependency group payload is not an Original"));
                };
                let imports = original_native_group_imports(product, demander.original_ordinal)
                    .ok_or_else(|| {
                        refuse("SOURCE dependency lacks its certified global imports")
                    })?;
                let definitions =
                    original_native_group_definitions(product, demander.original_ordinal)
                        .ok_or_else(|| {
                            refuse("SOURCE dependency lacks its certified group definitions")
                        })?;
                if definitions.globals().len() != imports.len() {
                    return Err(refuse(
                        "SOURCE dependency global slots differ from certified imports",
                    ));
                }
                for (index, import) in imports.iter().enumerate() {
                    crate::host_work::checkpoint()?;
                    operation
                        .charge(1)
                        .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
                    let ReceiptImportOwner::Retained {
                        identity,
                        generation,
                    } = import
                    else {
                        continue;
                    };
                    let global_index = u32::try_from(index).map_err(|_| {
                        refuse("SOURCE dependency global slot exceeds its wire index")
                    })?;
                    operation
                        .reserve::<OwnedRetainedOccurrence>(1)
                        .and_then(|_| {
                            operation.charge(
                                identity
                                    .unit
                                    .len()
                                    .saturating_add(identity.module.len())
                                    .saturating_add(identity.namespace.len())
                                    .saturating_add(identity.occurrence.len())
                                    .saturating_add(
                                        identity.record_parent.as_ref().map_or(0, String::len),
                                    ),
                            )
                        })
                        .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
                    let binding = view.acquired_original_group_retained_binding(
                        demander,
                        identity,
                        *generation,
                        operation,
                    )?;
                    retained.push(OwnedRetainedOccurrence {
                        demander,
                        global_index,
                        binding,
                        identity: identity.clone(),
                        generation: *generation,
                    });
                }
                let children = view.acquired_original_group_children(demander, operation)?;
                // The stack visits children in their issued edge order.
                pending.extend(children.into_iter().rev());
            }
            retained.sort();
            rows.push(SourceDependencyRow {
                reference,
                retained,
            });
        }
        Ok(Self {
            _primary: primary.clone(),
            _membership: membership,
            rows,
        })
    }

    pub(crate) fn rows(&self) -> &[SourceDependencyRow] {
        &self.rows
    }
}

impl SourceDependencyRow {
    pub(crate) fn reference(&self) -> &ReceiptImportOwner {
        &self.reference
    }

    pub(crate) fn retained_occurrences(
        &self,
    ) -> impl ExactSizeIterator<Item = NativeRetainedOccurrence<'_>> {
        self.retained
            .iter()
            .map(|occurrence| NativeRetainedOccurrence {
                demander: occurrence.demander,
                global_index: occurrence.global_index,
                binding: &occurrence.binding,
                identity: &occurrence.identity,
                generation: occurrence.generation,
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifact_inventory::CompilerInputRole;

    #[test]
    #[ignore = "requires the declared rooted compiler service"]
    fn source_dependency_observer_checks_actual_scope_without_native_acquisition() {
        let fixture =
            crate::artifacts::test_support::prepare_source_publication_entry_fixture().unwrap();
        let context = fixture
            .entry
            .products()
            .original_execution_evidence()
            .unwrap()
            .context()
            .clone();
        let primary = context
            .capture_world_primary_inputs(context.toolchain_identity_sha256())
            .unwrap();
        let owner = &fixture.root.descriptor.owner;
        let selected = context
            .compiler_source_support()
            .compiler_primary_input(owner)
            .unwrap()
            .unwrap();
        assert!(matches!(
            context.compiler_input_projection().snapshot_roles()[owner],
            CompilerInputRole::ReusableOriginal { .. }
        ));
        let ArtifactPayload::Original(product) = &fixture.root.payload else {
            unreachable!()
        };
        let binder = original_native_group_definitions(product, fixture.root_ordinals[1])
            .unwrap()
            .export_contracts()
            .map(Result::unwrap)
            .find(|contract| contract.identity.occurrence == "second")
            .unwrap()
            .identity
            .clone();
        let reference = ReceiptImportOwner::OriginalBinding {
            unit: owner.unit.clone(),
            module: owner.module.clone(),
            interface: selected.interface(),
            original: selected.original().unwrap(),
            binder,
        };
        let operation = Arc::new(InventoryOperation::new(Default::default()));
        let before = context.snapshot();
        let before_graph = context.artifact_view().capture_graph_selection();
        let observed = SourceDependencyBasis::observe(
            &primary,
            vec![reference.clone(), reference.clone()],
            &operation,
        )
        .unwrap();
        assert_eq!(
            observed.rows().len(),
            2,
            "ordered duplicate uses remain distinct reply rows"
        );
        assert!(observed
            .rows()
            .iter()
            .all(|row| row.reference() == &reference));
        assert_eq!(
            context.artifact_view().capture_graph_selection(),
            before_graph,
            "metadata observation does not change selected native roots"
        );

        let without_source = context
            .as_ref()
            .clone()
            .rebuild(|facts| {
                facts.source_scope = Default::default();
                facts.lexical = Default::default();
            })
            .finish()
            .unwrap();
        let without_source = without_source
            .capture_world_primary_inputs(context.toolchain_identity_sha256())
            .unwrap();
        let member = AuthenticatedOriginalBindings::issue(
            without_source.inputs().shared_compiler_source_support(),
            operation.clone(),
            [&reference],
        )
        .unwrap();
        assert!(
            member.get(&reference).is_some(),
            "exact pair membership alone is not SOURCE admission"
        );
        let error = SourceDependencyBasis::observe(&without_source, vec![reference], &operation)
            .err()
            .unwrap();
        assert!(
            matches!(error, CompileError::CompilerEvidence(error) if matches!(error.as_ref(), CertificationError::Mismatch("SOURCE dependency owner is outside admitted source scope")))
        );
        let after = context.snapshot();
        assert_eq!(after.native_availability, before.native_availability);
        assert_eq!(after.input_decisions, before.input_decisions);
        assert_eq!(
            context.artifact_view().capture_graph_selection(),
            before_graph
        );
    }
    #[test]
    #[ignore = "requires the declared rooted compiler service"]
    fn source_group_metadata_refuses_missing_and_ambiguous_exact_issuers() {
        use crate::artifact_inventory::{
            ArtifactInventory, ArtifactInventoryFailure, NativeArtifactDemand, NativeGroupKey,
        };
        let fixture =
            crate::artifacts::test_support::prepare_source_publication_entry_fixture().unwrap();
        let context = fixture
            .entry
            .products()
            .original_execution_evidence()
            .unwrap()
            .context()
            .clone();
        let inventory = ArtifactInventory::default();
        let base = inventory
            .admit_shared_with_demand(
                &inventory.empty_view(),
                context.artifact_view().entries(),
                NativeArtifactDemand::ScopeInterfaces,
            )
            .unwrap();
        let carrier = *base
            .capture_graph_selection()
            .namespace
            .iter()
            .find(|binding| binding.artifact == fixture.root.descriptor.id)
            .unwrap();
        let operation = InventoryOperation::new(Default::default());
        let missing = base
            .acquired_original_group_for_carrier(carrier, fixture.root_ordinals[0], &operation)
            .unwrap_err();
        assert!(
            matches!(missing, CompileError::ArtifactInventory(error) if matches!(error.failure,
            ArtifactInventoryFailure::NativeGroupUnavailable { artifact, original_ordinal }
                if artifact == carrier.artifact && original_ordinal == fixture.root_ordinals[0]))
        );
        let issue = |ordinals: &[u32]| {
            inventory
                .admit_recovery_selection(
                    &base,
                    vec![],
                    &ordinals
                        .iter()
                        .map(|original_ordinal| NativeGroupKey {
                            artifact: carrier.artifact,
                            original_ordinal: *original_ordinal,
                        })
                        .collect(),
                )
                .unwrap()
        };
        let left = issue(&fixture.root_ordinals[..1]);
        let right = issue(&fixture.root_ordinals);
        let left_group = left
            .acquired_original_group_for_carrier(carrier, fixture.root_ordinals[0], &operation)
            .unwrap();
        let right_group = right
            .acquired_original_group_for_carrier(carrier, fixture.root_ordinals[0], &operation)
            .unwrap();
        assert_ne!(
            left_group, right_group,
            "the control holds separate group issuances for the same exact carrier and ordinal"
        );
        let both = left.retain_custody(&right).unwrap();
        let before = both.capture_graph_selection();
        let ambiguous = both
            .acquired_original_group_for_carrier(carrier, fixture.root_ordinals[0], &operation)
            .unwrap_err();
        assert!(
            matches!(ambiguous, CompileError::ArtifactInventory(error) if matches!(error.failure,
            ArtifactInventoryFailure::BindingAmbiguity { artifact } if artifact == carrier.artifact))
        );
        assert_eq!(
            both.capture_graph_selection(),
            before,
            "refusal does not choose or withdraw a held issuer"
        );
        assert_eq!(
            left.acquired_original_group_for_carrier(carrier, fixture.root_ordinals[0], &operation)
                .unwrap(),
            left_group
        );
    }
    #[test]
    #[ignore = "requires the declared rooted compiler service"]
    fn source_observation_keeps_the_selected_historical_body_with_newer_custody() {
        let fixture =
            crate::artifacts::test_support::prepare_source_publication_entry_fixture().unwrap();
        let original_context = fixture
            .entry
            .products()
            .original_execution_evidence()
            .unwrap()
            .context()
            .clone();
        let ArtifactPayload::Original(old_product) = &fixture.root.payload else {
            unreachable!()
        };
        let source_path = fixture.includes[0].join("SourceOriginalSupport.hs");
        let source = std::fs::read_to_string(&source_path).unwrap();
        assert!(source.contains("first = 17"));
        std::fs::write(&source_path, source.replace("first = 17", "first = 19")).unwrap();
        let frozen = crate::artifacts::FrozenEntrySources::capture(
            &fixture.includes,
            &fixture.includes[0].join("PublicationTarget.hs"),
        )
        .unwrap();
        let directory = fixture.includes[0].parent().unwrap();
        let newer = crate::artifacts::test_support::prepare_frozen_production_entry(
            &frozen,
            directory,
            &directory.join("newer-source"),
        )
        .unwrap();
        let newer_context = newer
            .products()
            .original_execution_evidence()
            .unwrap()
            .context()
            .clone();
        let old_input = original_context
            .compiler_source_support()
            .compiler_primary_input(&fixture.root.descriptor.owner)
            .unwrap()
            .unwrap();
        let new_input = newer_context
            .compiler_source_support()
            .compiler_primary_input(&fixture.root.descriptor.owner)
            .unwrap()
            .unwrap();
        assert_ne!(
            old_input.original(),
            new_input.original(),
            "the second actual compile must issue another native body"
        );
        let ArtifactPayload::Original(new_product) = &new_input.entry().payload else {
            unreachable!()
        };
        assert_ne!(old_product.owner(), new_product.owner());
        let binder = original_native_group_definitions(old_product, fixture.root_ordinals[0])
            .unwrap()
            .export_contracts()
            .map(Result::unwrap)
            .find(|contract| contract.identity.occurrence == "first")
            .unwrap()
            .identity
            .clone();
        let reference = ReceiptImportOwner::OriginalBinding {
            unit: binder.unit.clone(),
            module: binder.module.clone(),
            interface: old_input.interface(),
            original: old_input.original().unwrap(),
            binder: binder.clone(),
        };
        let newer_reference = ReceiptImportOwner::OriginalBinding {
            unit: binder.unit.clone(),
            module: binder.module.clone(),
            interface: new_input.interface(),
            original: new_input.original().unwrap(),
            binder,
        };
        let historical = original_context
            .as_ref()
            .clone()
            .rebuild(|facts| {
                facts.compiler_support = facts
                    .compiler_support
                    .retain_custody(newer_context.artifact_view())
                    .unwrap();
            })
            .finish()
            .unwrap();
        assert!(historical
            .artifact_view()
            .observed_payload(&new_input.original().unwrap().artifact)
            .is_some());
        let primary = historical
            .capture_world_primary_inputs(historical.toolchain_identity_sha256())
            .unwrap();
        let operation = Arc::new(InventoryOperation::new(Default::default()));
        let observed =
            SourceDependencyBasis::observe(&primary, vec![reference.clone()], &operation).unwrap();
        assert_eq!(
            &observed._membership.get(&reference).unwrap().0,
            old_product.owner()
        );
        assert!(
            SourceDependencyBasis::observe(&primary, vec![newer_reference], &operation).is_err(),
            "newer byte custody cannot replace the selected exact source body"
        );
        assert!(
            SourceDependencyBasis::observe(&primary, vec![reference], &operation).is_ok(),
            "refusal preserves the historical basis"
        );
    }
}
