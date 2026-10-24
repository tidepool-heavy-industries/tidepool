//! Read-only SOURCE metadata from an admitted primary and its exact issued contribution.

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

use super::{
    ChosenNativeGroupSupport, NativeRetainedOccurrence, RetainedValueEvidence, WorldPrimaryInputs,
};

/// An observation keeps the same admitted SOURCE context, exact input selection
/// and accounting owner alive. It cannot authorize native acquisition or values.
pub(crate) struct SourceDependencyBasis {
    _primary: WorldPrimaryInputs,
    _membership: Arc<AuthenticatedOriginalBindings>,
    rows: Vec<SourceDependencyRow>,
}

pub(crate) struct SourceDependencyRow {
    reference: ReceiptImportOwner,
    support: ChosenNativeGroupSupport,
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
        query: crate::certified_products::native_demand::SourceDependencyQuery,
        operation: &Arc<InventoryOperation>,
    ) -> Result<Self, CompileError> {
        let references = query.into_originals();
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
        let mut rows = Vec::with_capacity(references.len());
        for reference in references {
            crate::host_work::checkpoint()?;
            let support = membership
                .available_group(&reference, operation)?
                .ok_or_else(|| {
                    let ReceiptImportOwner::OriginalBinding { original, .. } = &reference else {
                        return refuse("authenticated SOURCE dependency reference changed kind");
                    };
                    let Some((_, ordinal)) = membership.get(&reference) else {
                        return refuse("SOURCE dependency lacks authenticated pair membership");
                    };
                    crate::artifact_inventory::admission_failure(
                    crate::artifact_inventory::ArtifactInventoryFailure::NativeGroupUnavailable {
                        artifact: original.artifact,
                        original_ordinal: *ordinal,
                    },
                )
                })?;
            let scope = crate::artifact_inventory::SelectedOriginalGroupScope::contribution(
                support.contribution(),
            );
            let root = support.binding();
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
                operation
                    .charge(1)
                    .map_err(|error| CompileError::CompilerEvidence(Box::new(error.into())))?;
                let entry = scope.group_payload(demander)?;
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
                    let binding =
                        scope.retained_binding(demander, identity, *generation, operation)?;
                    retained.push(OwnedRetainedOccurrence {
                        demander,
                        global_index,
                        binding,
                        identity: identity.clone(),
                        generation: *generation,
                    });
                }
                let children = scope.children(demander, operation)?;
                // The stack visits children in their issued edge order.
                pending.extend(children.into_iter().rev());
            }
            retained.sort();
            rows.push(SourceDependencyRow {
                reference,
                support,
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

    /// Compare a runtime row's immutable type/evidence with one exact observed
    /// terminal. Runtime access still belongs to its independently scoped owner.
    pub(crate) fn select_retained_values_for_use(
        &self,
        reference: &ReceiptImportOwner,
        requested: &[NativeRetainedOccurrence<'_>],
    ) -> Result<Vec<RetainedValueEvidence>, CompileError> {
        let observed = self
            .rows
            .iter()
            .find(|row| row.reference() == reference)
            .ok_or_else(|| refuse("SOURCE terminal has no issued exact use"))?;
        requested
            .iter()
            .map(|occurrence| {
                let found = observed.retained.binary_search_by(|candidate| {
                    NativeRetainedOccurrence {
                        demander: candidate.demander,
                        global_index: candidate.global_index,
                        binding: &candidate.binding,
                        identity: &candidate.identity,
                        generation: candidate.generation,
                    }
                    .cmp(occurrence)
                });
                if found.is_err() {
                    return Err(refuse("SOURCE terminal exceeds its issued exact use"));
                }
                let entry = crate::artifact_inventory::SelectedOriginalGroupScope::contribution(
                    observed.support.contribution(),
                )
                .retained_payload(occurrence.demander, *occurrence.binding)?;
                Ok(RetainedValueEvidence {
                    entry: Arc::clone(entry),
                })
            })
            .collect()
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

    fn source_query(
        references: Vec<ReceiptImportOwner>,
        operation: &InventoryOperation,
    ) -> crate::certified_products::native_demand::SourceDependencyQuery {
        use crate::certified_products::native_demand::DependencyQuery;
        use ciborium::value::Value;
        let binding = |binding: &ArtifactBinding| {
            Value::Array(vec![
                Value::Bytes(binding.artifact.0.to_vec()),
                Value::Bytes(binding.selection.0.to_vec()),
            ])
        };
        let uses = references
            .iter()
            .map(|reference| {
                let ReceiptImportOwner::OriginalBinding {
                    unit,
                    module,
                    interface,
                    original,
                    binder,
                } = reference
                else {
                    panic!("SOURCE fixture requests an exact Original use");
                };
                Value::Array(vec![
                    Value::Text(unit.clone()),
                    Value::Text(module.clone()),
                    binding(interface),
                    binding(original),
                    Value::Array(vec![
                        Value::Text(binder.unit.clone()),
                        Value::Text(binder.module.clone()),
                        Value::Text(binder.namespace.clone()),
                        Value::Text(binder.occurrence.clone()),
                        binder
                            .record_parent
                            .as_ref()
                            .map_or(Value::Null, |parent| Value::Text(parent.clone())),
                    ]),
                ])
            })
            .collect();
        let query = Value::Array(vec![
            Value::Text("TPDEPENDENCYQUERY1".into()),
            Value::Integer(0u64.into()),
            Value::Array(uses),
        ]);
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(&query, &mut bytes).unwrap();
        let DependencyQuery::Source(query) = DependencyQuery::decode(&bytes, operation).unwrap()
        else {
            unreachable!()
        };
        query
    }

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
            source_query(vec![reference.clone()], &operation),
            &operation,
        )
        .unwrap();
        assert_eq!(
            observed.rows().len(),
            1,
            "the observed row retains the exact decoded SOURCE use"
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
        let error = SourceDependencyBasis::observe(
            &without_source,
            source_query(vec![reference], &operation),
            &operation,
        )
        .err()
        .unwrap();
        assert!(
            matches!(error, CompileError::CompilerEvidence(error) if matches!(error.as_ref(), CertificationError::Mismatch("SOURCE dependency owner is outside admitted source scope")))
        );
        let after = context.snapshot();
        assert_eq!(after.native_availability, before.native_availability);
        assert_eq!(after.recorded_rows(), before.recorded_rows());
        assert_eq!(
            context.artifact_view().capture_graph_selection(),
            before_graph
        );
    }
    #[test]
    #[ignore = "requires the declared rooted compiler service"]
    fn acquired_group_scope_checks_explicit_membership_without_union_ambiguity() {
        use crate::artifact_inventory::{
            ArtifactInventory, NativeArtifactDemand, NativeGroupKey, SelectedOriginalGroupScope,
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
        let operation = Arc::new(InventoryOperation::new(Default::default()));
        let primary = context
            .capture_world_primary_inputs(context.toolchain_identity_sha256())
            .unwrap();
        let selected = context
            .compiler_source_support()
            .compiler_primary_input(&fixture.root.descriptor.owner)
            .unwrap()
            .unwrap();
        let ArtifactPayload::Original(product) = &fixture.root.payload else {
            unreachable!()
        };
        let issue = |ordinals: &[u32]| {
            let mut complete = BTreeSet::new();
            for ordinal in ordinals {
                let binder = original_native_group_definitions(product, *ordinal)
                    .unwrap()
                    .export_contracts()
                    .map(Result::unwrap)
                    .find(|contract| {
                        contract.identity.module == fixture.root.descriptor.owner.module
                    })
                    .unwrap()
                    .identity
                    .clone();
                let reference = ReceiptImportOwner::OriginalBinding {
                    unit: fixture.root.descriptor.owner.unit.clone(),
                    module: fixture.root.descriptor.owner.module.clone(),
                    interface: selected.interface(),
                    original: selected.original().unwrap(),
                    binder,
                };
                let membership = AuthenticatedOriginalBindings::issue(
                    primary.inputs().shared_compiler_source_support(),
                    operation.clone(),
                    [&reference],
                )
                .unwrap();
                let chosen = membership
                    .available_group(&reference, &operation)
                    .unwrap()
                    .unwrap();
                let issued = SelectedOriginalGroupScope::contribution(chosen.contribution());
                let mut pending = vec![chosen.binding()];
                while let Some(group) = pending.pop() {
                    if complete.insert(NativeGroupKey {
                        artifact: group.binding.artifact(),
                        original_ordinal: group.original_ordinal,
                    }) {
                        pending.extend(issued.children(group, &operation).unwrap());
                    }
                }
            }
            inventory
                .admit_recovery_selection(&base, vec![], &complete)
                .unwrap()
        };
        let left = issue(&fixture.root_ordinals[..1]);
        let right = issue(&fixture.root_ordinals);
        let group = |view: &crate::artifact_inventory::ArtifactView| {
            *SelectedOriginalGroupScope::selected(view)
                .bindings(&operation)
                .unwrap()
                .iter()
                .find(|group| {
                    group.binding.artifact() == fixture.root.descriptor.id
                        && group.original_ordinal == fixture.root_ordinals[0]
                })
                .unwrap()
        };
        let left_group = group(&left);
        let right_group = group(&right);
        assert_ne!(
            left_group, right_group,
            "control holds independent group issuers"
        );
        let explicit = BTreeSet::from([left_group]);
        let missing = SelectedOriginalGroupScope::acquired(&base, &explicit, &operation)
            .err()
            .unwrap();
        assert!(
            matches!(missing, CompileError::ArtifactInventory(error) if matches!(error.failure,
            crate::artifact_inventory::ArtifactInventoryFailure::NativeGroupUnavailable { artifact, original_ordinal }
                if artifact == left_group.binding.artifact() && original_ordinal == left_group.original_ordinal)),
            "payload custody alone cannot authenticate a group"
        );
        let both = left.retain_custody(&right).unwrap();
        let before = both.capture_graph_selection();
        let scope = SelectedOriginalGroupScope::acquired(&both, &explicit, &operation).unwrap();
        assert!(scope.group_payload(left_group).is_ok());
        assert!(scope.children(left_group, &operation).is_ok());
        assert!(scope.group_payload(right_group).is_err());
        assert!(scope.children(right_group, &operation).is_err());
        assert!(
            SelectedOriginalGroupScope::selected(&both)
                .group_payload(right_group)
                .is_err(),
            "a selected query cannot use a custody-only group"
        );
        let right_scope = SelectedOriginalGroupScope::selected(&right);
        let parent = right_scope
            .bindings(&operation)
            .unwrap()
            .into_iter()
            .find(|group| {
                group.binding.artifact() == fixture.root.descriptor.id
                    && group.original_ordinal == fixture.root_ordinals[1]
            })
            .unwrap();
        assert!(
            !right_scope.children(parent, &operation).unwrap().is_empty(),
            "the incomplete-set refusal has a real exact child"
        );
        let incomplete = BTreeSet::from([parent]);
        assert!(
            SelectedOriginalGroupScope::acquired(&both, &incomplete, &operation)
                .unwrap()
                .children(parent, &operation)
                .is_err(),
            "acquired traversal requires complete explicit children"
        );
        assert_eq!(scope.bindings(&operation).unwrap(), vec![left_group]);
        assert_eq!(both.capture_graph_selection(), before);
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
        let observed = SourceDependencyBasis::observe(
            &primary,
            source_query(vec![reference.clone()], &operation),
            &operation,
        )
        .unwrap();
        assert_eq!(
            &observed._membership.get(&reference).unwrap().0,
            old_product.owner()
        );
        assert!(
            SourceDependencyBasis::observe(
                &primary,
                source_query(vec![newer_reference], &operation),
                &operation
            )
            .is_err(),
            "newer byte custody cannot replace the selected exact source body"
        );
        assert!(
            SourceDependencyBasis::observe(
                &primary,
                source_query(vec![reference], &operation),
                &operation
            )
            .is_ok(),
            "refusal preserves the historical basis"
        );
    }
}
