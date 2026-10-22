//! Data-only mutations at the actual World stage projection boundary.

use super::*;

pub(super) fn assert_actual_type_anchors(
    stage_inputs: &Arc<RequestCompilerInputs>,
    actual: &DemandedWorldCompilerInputs,
    prefix_groups: &DemandCertifiedGroups,
    receipt: &crate::certified_products::CertifiedReceipt,
    products: &crate::certified_products::ParsedModuleProducts,
    produced: &crate::checked_cell::ProducedValueTypeInterfaces,
) {
    let prior = produced.before_segment();
    let current_outputs = produced.required().count();
    for output in produced.required() {
        assert!(
            !prior.selects_artifact(output.artifact_id()),
            "current produced TYPE output is outside the prior anchor owner"
        );
    }
    let prior_returned = prior
        .interfaces()
        .filter(|output| {
            let interface = output.interface();
            receipt
                .finalization
                .value_interfaces
                .contains_key(&(interface.unit().to_owned(), interface.module().to_owned()))
        })
        .collect::<Vec<_>>();
    if prior_returned.is_empty() {
        eprintln!("actual World TYPE anchor controls: stage {}, prior 0, omitted 0, wrong facts 0; current structural checks {current_outputs}", actual.initial.stage().index());
        return;
    }
    let project = |observed: &crate::certified_products::CertifiedReceipt| {
        stage_inputs.source_support.project_world_stage_inputs(
            &actual.demand.demand,
            &actual.demand.native_selection.availability,
            &actual.demand.permission_delta,
            prefix_groups,
            &actual.initial,
            &actual.original_bindings,
            observed,
            products,
            produced,
            actual.initial.operation(),
        )
    };
    let assert_choices_unchanged = |support: &crate::artifact_inventory::CompilerSourceSupport| {
        assert_eq!(support.projection(), actual.support.projection());
        assert_eq!(
            support.recorded_input_decisions(),
            actual.support.recorded_input_decisions()
        );
        assert_eq!(
            support.artifacts().selected_native_groups(),
            actual.support.artifacts().selected_native_groups()
        );
        assert_eq!(
            support.artifacts().capture_graph_selection().native_groups,
            actual
                .support
                .artifacts()
                .capture_graph_selection()
                .native_groups
        );
    };
    let mut omitted = receipt.clone();
    for output in &prior_returned {
        let interface = output.interface();
        assert!(omitted
            .finalization
            .value_interfaces
            .remove(&(interface.unit().to_owned(), interface.module().to_owned(),))
            .is_some());
    }
    let base = project(&omitted).expect("omitted TYPE metadata adds no anchor");
    assert_choices_unchanged(&base);
    let base_graph = base.artifacts().capture_graph_selection();
    let mut exercised = 0;
    for output in prior_returned {
        let interface = output.interface();
        let owner = identity(interface.unit(), interface.module());
        let selected = stage_inputs
            .source_support
            .compiler_primary_input(&owner)
            .unwrap()
            .expect("actual prior output retains its selected primary TYPE owner");
        let binding = selected.interface();
        assert_eq!(binding.artifact, output.artifact_id());
        if base_graph.bindings.contains(&binding) {
            // An ordinary demanded dependency already owns this row. Omitting
            // its metadata cannot establish an absence or test this anchor.
            continue;
        }
        assert!(!base_graph.bindings.contains(&binding));
        let actual_graph = actual.support.artifacts().capture_graph_selection();
        assert!(actual_graph.namespace.contains(&binding));
        assert!(actual_graph.bindings.contains(&binding));
        let key = (owner.unit.clone(), owner.module.clone());
        for change_requirements in [false, true] {
            let mut changed = receipt.clone();
            let row = changed.finalization.value_interfaces.get_mut(&key).unwrap();
            if change_requirements {
                row.interface_requirements.insert(
                    (
                        owner.unit.clone(),
                        format!("{}_unexpected_type", owner.module),
                    ),
                    [255; 32],
                );
            } else {
                row.interface.sha256[0] ^= 1;
            }
            let error = project(&changed)
                .err()
                .expect("changed actual TYPE facts must refuse");
            assert!(
                matches!(&error, CompileError::ExtractFailed(detail)
                if detail == "artifact inventory: World prior produced TYPE metadata differs from its captured owner"),
                "TYPE anchor must refuse at its owning exact-fact check: {error:?}"
            );
        }
        let retry = project(receipt).expect("exact TYPE metadata succeeds after refusal");
        assert_choices_unchanged(&retry);
        assert!(retry
            .artifacts()
            .capture_graph_selection()
            .namespace
            .contains(&binding));
        exercised += 1;
        // One absent-from-base witness proves this boundary without repeating
        // every immutable prefix projection in a large produced inventory.
        break;
    }
    eprintln!("actual World TYPE anchor controls: stage {}, prior {exercised}, omitted {exercised}, wrong facts {}; current structural checks {current_outputs}", actual.initial.stage().index(), exercised * 2);
}

/// The named repeated-source fixture chooses an actually available prior group.
/// These positive execution checks do not impose native demand on Source metadata.
/// Raw observation mutations below do not create a new certificate or choice.
pub(super) fn assert_actual_available_prior_source_inputs(
    continuation: &WorldProgramContinuation,
    inputs: &DemandedWorldCompilerInputs,
    selection: &crate::certified_products::CertifiedSourceSelection,
    observed: &[crate::certified_products::ReceiptImportOwner],
) {
    use crate::certified_products::{
        CertificationError, CertifiedSourceSelection, ReceiptImportOwner,
    };

    let mut exercised = 0;
    for reference in observed {
        let ReceiptImportOwner::Source {
            unit,
            module,
            original_ordinal,
            ..
        } = reference
        else {
            unreachable!("only actual unversioned Source observations are borrowed");
        };
        let owner = identity(unit, module);
        let Some(program) = inputs.program_support.as_ref() else {
            break;
        };
        let Some(receipt) = program
            .imports
            .get(&owner)
            .and_then(|row| row.physical_product.as_ref())
        else {
            continue;
        };
        let compiler_entry = program
            .compiler_support
            .implementation_entry(&owner)
            .expect("actual produced Source has its held compiler entry");
        let expected_entry = receipt
            .row()
            .target_original(receipt.row().original_owner())
            .expect("actual produced Source retains its physical Original");
        let ArtifactPayload::Original(product) = &expected_entry.payload else {
            panic!("actual produced Source has its Original payload");
        };
        let key = crate::artifact_inventory::NativeGroupKey {
            artifact: expected_entry.descriptor.id,
            original_ordinal: *original_ordinal,
        };
        let expected = receipt.groups(key).collect::<BTreeSet<_>>();
        if expected.is_empty() {
            continue;
        }
        let (entry, ordinal) = selection
            .target_source_entry(reference)
            .expect("actual prior unversioned Source resolves after physical issuance");
        assert_eq!(ordinal, *original_ordinal);
        assert_eq!(entry.descriptor.id, expected_entry.descriptor.id);
        let prior = selection
            .target_prior_produced_group(key, &entry)
            .unwrap()
            .expect("actual prior Source retains produced provenance");
        assert_eq!(prior.groups().collect::<BTreeSet<_>>(), expected);
        assert!(matches!(
            selection.target_source_origin(key, &entry).unwrap(),
            crate::certified_products::TargetSourceOrigin::PriorProgram(_)
        ));
        if matches!(compiler_entry.payload, ArtifactPayload::Canonical(_)) {
            assert!(
                !selection.permits_native_group(key.artifact, product.owner(), ordinal),
                "a canonical compiler role does not gain native permission from prior membership"
            );
        } else {
            assert!(selection.permits_native_group(key.artifact, product.owner(), ordinal));
        }
        for binding in expected {
            assert!(std::ptr::eq(
                prior
                    .support(binding)
                    .expect("prior Source carries exact support"),
                receipt
                    .support(key, binding)
                    .expect("physical receipt owns exact support"),
            ));
        }

        let mut wrong_version = reference.clone();
        let mut version = product.owner().module_version.clone();
        version.0[0] ^= 1;
        if let ReceiptImportOwner::Source { module_version, .. } = &mut wrong_version {
            *module_version = Some(version);
        }
        assert!(matches!(
            selection.target_source_entry(&wrong_version),
            Err(CertificationError::Mismatch(
                "source module version" | "prior Source module version"
            ))
        ));
        let mut wrong_binder = reference.clone();
        if let ReceiptImportOwner::Source { binder, .. } = &mut wrong_binder {
            binder.occurrence.push_str("_unissued_binder");
        }
        assert!(matches!(
            selection.target_source_entry(&wrong_binder),
            Err(CertificationError::Mismatch(
                "target Source exact native membership"
            ))
        ));

        // This is an absent-body ordinal control, not a Partial-offer oracle.
        let absent = crate::artifact_inventory::NativeGroupKey {
            original_ordinal: u32::MAX,
            ..key
        };
        assert!(!entry
            .native_group_ordinals
            .contains(&absent.original_ordinal));
        let mut wrong_ordinal = reference.clone();
        if let ReceiptImportOwner::Source {
            original_ordinal, ..
        } = &mut wrong_ordinal
        {
            *original_ordinal = absent.original_ordinal;
        }
        assert!(matches!(
            selection.target_source_entry(&wrong_ordinal),
            Err(CertificationError::Mismatch(
                "target Source exact native membership"
            ))
        ));
        assert!(!selection.permits_native_group(
            absent.artifact,
            product.owner(),
            absent.original_ordinal
        ));
        assert!(selection
            .target_prior_produced_group(absent, &entry)
            .unwrap()
            .unwrap()
            .groups()
            .next()
            .is_none());

        // The same carried compiler support and availability, without the
        // actual program-source row, cannot manufacture prior-stage origin.
        let carried = CertifiedSourceSelection::from_compiler_inputs(
            &continuation.inputs,
            inputs.initial.operation(),
        )
        .expect("ordinary carried selection remains valid");
        assert!(matches!(
            carried.target_source_entry(reference),
            Err(CertificationError::Mismatch(
                "missing cached source version" | "unversioned source lacks compiler selection"
            ))
        ));
        assert!(carried
            .target_prior_produced_group(key, &entry)
            .unwrap()
            .is_none());
        let initial = CertifiedSourceSelection::from_compiler_inputs(
            inputs.initial.initial_inputs(),
            inputs.initial.operation(),
        )
        .expect("actual initial request selection remains valid");
        assert!(initial
            .target_prior_produced_group(key, &entry)
            .unwrap()
            .is_none());
        assert_eq!(
            selection
                .target_prior_produced_group(key, &entry)
                .unwrap()
                .unwrap()
                .groups()
                .collect::<BTreeSet<_>>(),
            receipt.groups(key).collect::<BTreeSet<_>>()
        );
        exercised += 1;
        break;
    }
    assert!(
        exercised > 0,
        "named prior Source fixture owns an actually available group"
    );
    eprintln!("actual World prior Source controls: stage {}, prior {exercised}, wrong version {exercised}, wrong binder {exercised}, absent ordinal {exercised}, carried provenance {exercised}", inputs.initial.stage().index());
}

/// A completed dormant body can publish future permission while its current
/// stage still refuses execution of an available, unselected Original use.
pub(super) fn assert_completed_current_stage_ceiling(
    inputs: &DemandedWorldCompilerInputs,
    originals: &crate::certified_products::CertifiedSegmentOriginals,
) {
    use crate::certified_products::{CertificationError, ReceiptImportOwner};

    let mut exercised = 0;
    if let Some(owner) = inputs.initial.stage().identity().original_owner() {
        let selection = originals.source_selection();
        let physical_inputs = originals.physical_inputs();
        let physical = physical_inputs.issued_artifacts();
        'current: for entry in originals.artifact_entries() {
            if entry.descriptor.owner != *owner {
                continue;
            }
            let ArtifactPayload::Original(product) = &entry.payload else {
                continue;
            };
            for &ordinal in &entry.native_group_ordinals {
                let key = crate::artifact_inventory::NativeGroupKey {
                    artifact: entry.descriptor.id,
                    original_ordinal: ordinal,
                };
                let Some(current) = selection.current_product(key, entry).unwrap() else {
                    continue;
                };
                for import in current.nominal_imports().unwrap() {
                    let reference = import.reference();
                    if !matches!(reference, ReceiptImportOwner::OriginalBinding { .. })
                        || current
                            .available_original_group(reference, inputs.initial.operation())
                            .unwrap()
                            .is_none()
                        || inputs
                            .original_bindings
                            .executable(reference, inputs.initial.operation())
                            .unwrap()
                            .is_some()
                    {
                        continue;
                    }
                    let receipt = selection
                        .produced_source_receipt(owner, physical)
                        .unwrap()
                        .expect("actual current product has a completed physical issuer");
                    let own = receipt.groups(key).collect::<BTreeSet<_>>();
                    assert!(
                        !own.is_empty(),
                        "dormant current group is already physically available"
                    );
                    assert!(selection.permits_native_group(key.artifact, product.owner(), ordinal));
                    assert!(
                        inputs
                            .original_bindings
                            .executable(reference, inputs.initial.operation())
                            .unwrap()
                            .is_none(),
                        "actual stage does not select the available Original use"
                    );
                    let roots = physical.capture_graph_selection().roots;
                    let native = physical.capture_graph_selection().native_groups;
                    let decisions = inputs.support.recorded_input_decisions().clone();
                    let refuse_shortcut =
                        || {
                            let error = current.completed_for_target(physical).err().expect(
                                "physical completion cannot bypass the actual stage ceiling",
                            );
                            assert!(matches!(&error, CompileError::CompilerEvidence(error)
                            if matches!(error.as_ref(), CertificationError::Mismatch(
                                "current Original use exceeds actual stage demand"))),
                            "completed target must refuse at the actual stage ceiling: {error:?}");
                        };
                    refuse_shortcut();
                    let publication = selection
                        .produced_authored_original_publication(
                            physical_inputs.physical_cohort().unwrap(),
                            owner,
                        )
                        .expect("dormant produced body can publish its existing future permission");
                    assert!(Arc::ptr_eq(
                        publication.produced_availability().unwrap(),
                        &receipt
                    ));
                    let published = publication.native_closure().capture_graph_selection();
                    for binding in &own {
                        assert!(published.native_groups.contains(binding));
                    }
                    refuse_shortcut();
                    assert_eq!(physical.capture_graph_selection().roots, roots);
                    assert_eq!(physical.capture_graph_selection().native_groups, native);
                    assert_eq!(inputs.support.recorded_input_decisions(), &decisions);
                    assert_eq!(receipt.groups(key).collect::<BTreeSet<_>>(), own);
                    assert!(selection.permits_native_group(key.artifact, product.owner(), ordinal));
                    assert!(inputs
                        .original_bindings
                        .executable(reference, inputs.initial.operation())
                        .unwrap()
                        .is_none());
                    exercised += 1;
                    break 'current;
                }
            }
        }
    }
    eprintln!("actual World completed-stage ceiling: stage {}, refusal {exercised}, publication {exercised}, retry refusal {exercised}", inputs.initial.stage().index());
}
