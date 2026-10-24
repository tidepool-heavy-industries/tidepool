//! Authentication controls entered only with actual compiler-issued targets.

use super::*;
use crate::recovery_artifacts::PackageInterfaceValidation;
use tidepool_repr::native_code::NativeCodeComponentFacts;

#[derive(Debug, Default)]
pub(crate) struct AuthenticationCoverage {
    pub home_scope: bool,
    pub source_selector: bool,
    pub retained_selector: bool,
    pub package_leaf: bool,
}
impl AuthenticationCoverage {
    pub fn merge(&mut self, other: Self) {
        self.home_scope |= other.home_scope;
        self.source_selector |= other.source_selector;
        self.retained_selector |= other.retained_selector;
        self.package_leaf |= other.package_leaf;
    }
}

pub(crate) fn assert_actual_target_authentication(
    target: &LinkedNativeTarget,
) -> AuthenticationCoverage {
    let mut coverage = AuthenticationCoverage::default();
    let operation = InventoryOperation::new(Default::default());
    let actual = target.closure();
    let exhausted =
        InventoryOperation::new(tidepool_repr::execution_schema::InventoryDecodeLimits {
            max_work: 0,
            ..Default::default()
        });
    assert!(
        matches!(encode_record(target.closure().recorded_facts(), &exhausted),
            Err(CompileError::CompilerEvidence(error)) if matches!(error.as_ref(),
                crate::certified_products::CertificationError::Product(
                    tidepool_repr::execution_schema::ParseError::LimitExceeded(_)
                ))
        ),
        "record byte admission preserves a typed operational refusal"
    );
    let nodes = actual
        .occurrences
        .values()
        .map(|node| node.as_ref().clone())
        .collect::<Vec<_>>();
    let reissued = AcquiredNativeClosure::from_occurrences(
        nodes.clone(),
        actual.artifacts().inventory().empty_view(),
        &operation,
    )
    .expect("actual acquired rows pass the same live/restore gate");
    assert_eq!(reissued.identity(), actual.identity());
    for selected in target.acquired() {
        for (index, import) in selected.imports().enumerate() {
            let AcquiredNativeImport::Retained {
                requirement,
                binding,
            } = import
            else {
                continue;
            };
            let index = u32::try_from(index).unwrap();
            let NativeOccurrenceId::Home(demander) = selected.occurrence() else {
                panic!("only HOME owns retained leaves")
            };
            let verified = target
                .verify_retained_slot(&selected, index, binding, &requirement)
                .expect("the actual target issues its selected retained slot witness");
            let (owner, slot, selected_binding, leaf) = match verified {
                VerifiedRetainedSlotDemander::CurrentStage(witness) => {
                    assert!(matches!(
                        target.sources[&demander],
                        TargetHomeSource::CurrentStage(_)
                    ));
                    assert_eq!(witness.demander(), demander);
                    (
                        witness.owner(),
                        witness.global_index(),
                        witness.binding(),
                        witness.leaf(),
                    )
                }
                VerifiedRetainedSlotDemander::PriorProgramStage(witness) => {
                    assert!(matches!(
                        target.sources[&demander],
                        TargetHomeSource::PriorProgramStage(_)
                    ));
                    assert_eq!(witness.demander(), demander);
                    (
                        witness.owner(),
                        witness.global_index(),
                        witness.binding(),
                        witness.leaf(),
                    )
                }
                VerifiedRetainedSlotDemander::HistoricalInput(witness) => {
                    assert!(matches!(
                        target.sources[&demander],
                        TargetHomeSource::HistoricalInput
                    ));
                    assert_eq!(witness.demander(), demander);
                    (
                        witness.owner(),
                        witness.global_index(),
                        witness.binding(),
                        witness.leaf(),
                    )
                }
            };
            assert!(std::ptr::eq(owner, &selected));
            assert_eq!(
                (slot, selected_binding, leaf),
                (index, binding, &requirement)
            );
            let foreign = reissued
                .get(selected.occurrence())
                .unwrap()
                .reborrow(&target.sources);
            assert_eq!(foreign.code(), selected.code());
            assert!(target.verify_retained_slot(&foreign, index, binding, &requirement).is_err(),
                "equal-looking code and source map cannot replace the actual acquired closure issuance");
            assert!(target
                .verify_retained_slot(
                    &selected,
                    selected.imports().len().try_into().unwrap(),
                    binding,
                    &requirement
                )
                .is_err());
            let mut crossed = binding;
            crossed.selection.0[0] ^= 1;
            assert!(target
                .verify_retained_slot(&selected, index, crossed, &requirement)
                .is_err());
            let crossed = NativeRetainedRequirement::new(
                requirement.identity().clone(),
                requirement.generation() ^ 1,
                requirement.type_artifact().copied(),
            );
            assert!(target
                .verify_retained_slot(&selected, index, binding, &crossed)
                .is_err());
        }
    }

    if let Some(package) = nodes
        .iter()
        .find(|node| matches!(node.occurrence(), NativeOccurrenceId::Package { .. }))
    {
        let mut data = serde_json::to_value(package.occurrence()).unwrap();
        data["Package"]["unrecognized"] = serde_json::Value::Bool(true);
        assert!(
            serde_json::from_value::<NativeOccurrenceId>(data).is_err(),
            "persisted occurrence selectors reject unknown proof fields"
        );
    }
    let components = target
        .acquired()
        .map(|selected| {
            let component = selected.code().component();
            (
                *component.identity(),
                Arc::clone(component.snapshot_record()),
            )
        })
        .collect::<BTreeMap<_, Arc<NativeCodeComponentFacts>>>();
    let packages = target
        .acquired()
        .filter_map(|selected| match selected.grant() {
            NativeCodeGrant::Package { product, .. } => {
                Some((product.artifact_id(), Arc::clone(product)))
            }
            _ => None,
        })
        .collect::<BTreeMap<_, _>>();
    let records = BTreeMap::from([(*actual.identity(), Arc::clone(actual.recorded_facts()))]);
    let mut validation = PackageInterfaceValidation::default();
    let restore = |validation: &mut PackageInterfaceValidation| {
        restore_acquired_native_closures(
            &components,
            &records,
            actual.artifacts(),
            &packages,
            validation,
        )
        .expect("actual captured grants restore through the same component and acquisition issuers")
        .pop()
        .unwrap()
    };
    let first = restore(&mut validation);
    let second = restore(&mut validation);
    assert_eq!(first.identity(), actual.identity());
    assert_eq!(second.recorded_facts(), actual.recorded_facts());
    for selected in first.acquired() {
        let repeated = second.get(selected.occurrence()).unwrap();
        assert!(
            Arc::ptr_eq(selected.code().component(), repeated.code().component()),
            "distinct restoration roots share canonical immutable component owners"
        );
        assert!(
            !target.owns_acquired(&selected),
            "restore cannot manufacture receiving-target source authority"
        );
        for (index, import) in selected.imports().enumerate() {
            if let AcquiredNativeImport::Retained {
                requirement,
                binding,
            } = import
            {
                assert!(target
                    .verify_retained_slot(
                        &selected,
                        index.try_into().unwrap(),
                        binding,
                        &requirement
                    )
                    .is_err());
            }
        }
    }

    if let Some(identity) = components.keys().next() {
        let mut missing = components.clone();
        missing.remove(identity);
        assert!(
            restore_acquired_native_closures(
                &missing,
                &records,
                actual.artifacts(),
                &packages,
                &mut validation
            )
            .is_err(),
            "a populated operation cache cannot fill absent current component facts"
        );
    }
    if let Some(identity) = packages.keys().next() {
        let mut missing = packages.clone();
        missing.remove(identity);
        assert!(
            restore_acquired_native_closures(
                &components,
                &records,
                actual.artifacts(),
                &missing,
                &mut validation
            )
            .is_err(),
            "a populated operation cache cannot fill absent exact package acquisition"
        );
    }

    // Mutated records remain DATA until they enter the authenticated restore
    // owner. Recommit each mutation so a stale outer digest cannot mask a
    // missing exact-scope, slot or product check.
    let refuse_record = |mut facts: NativeAcquiredClosureFacts| {
        facts.occurrences.sort_by_key(|row| row.occurrence);
        let bytes = encode_record(&facts, &operation).unwrap();
        let mut hash = Sha256::new();
        hash.update(b"Tidepool acquired native closure v1\0");
        hash.update(&bytes);
        let records = BTreeMap::from([(hash.finalize().into(), Arc::new(facts))]);
        assert!(
            restore_acquired_native_closures(
                &components,
                &records,
                actual.artifacts(),
                &packages,
                &mut PackageInterfaceValidation::default(),
            )
            .is_err(),
            "altered exact acquisition must refuse with a valid outer commitment"
        );
    };
    if let Some(home) = actual
        .recorded_facts()
        .occurrences
        .iter()
        .find(|row| matches!(row.occurrence, NativeOccurrenceId::Home(_)))
    {
        coverage.home_scope = true;
        let mut forged = actual.recorded_facts().as_ref().clone();
        let node = forged
            .occurrences
            .iter_mut()
            .find(|row| row.occurrence == home.occurrence)
            .unwrap();
        let NativeOccurrenceId::Home(mut binding) = node.occurrence else {
            unreachable!()
        };
        let mut data = serde_json::to_value(binding).unwrap();
        let unchanged: NativeGroupBinding = serde_json::from_value(data.clone()).unwrap();
        assert_eq!(
            serde_json::to_vec(&unchanged).unwrap(),
            serde_json::to_vec(&binding).unwrap()
        );
        let byte = data["binding"]["selection"][0].as_u64().unwrap();
        data["binding"]["selection"][0] = serde_json::Value::from(byte ^ 1);
        binding = serde_json::from_value(data).unwrap();
        node.occurrence = NativeOccurrenceId::Home(binding);
        refuse_record(forged);
    }
    for selector_kind in [false, true] {
        let slot = actual
            .recorded_facts()
            .occurrences
            .iter()
            .enumerate()
            .find_map(|(node_index, node)| {
                if !matches!(node.occurrence, NativeOccurrenceId::Home(_)) {
                    return None;
                }
                node.imports
                    .iter()
                    .enumerate()
                    .find_map(|(slot, selector)| {
                        let matches = if selector_kind {
                            matches!(selector, NativeGrantSelector::Retained { .. })
                        } else {
                            matches!(
                                selector,
                                NativeGrantSelector::Code(NativeOccurrenceId::Home(_))
                            )
                        };
                        matches.then_some((node_index, slot))
                    })
            });
        let Some((node_index, slot)) = slot else {
            continue;
        };
        if selector_kind {
            coverage.retained_selector = true;
        } else {
            coverage.source_selector = true;
        }
        let mut forged = actual.recorded_facts().as_ref().clone();
        let mut selectors = forged.occurrences[node_index].imports.to_vec();
        match &mut selectors[slot] {
            NativeGrantSelector::Code(NativeOccurrenceId::Home(binding)) => {
                binding.original_ordinal ^= 1
            }
            NativeGrantSelector::Retained { binding } => binding.selection.0[0] ^= 1,
            _ => unreachable!(),
        }
        forged.occurrences[node_index].imports = selectors.into();
        refuse_record(forged);
    }
    if let Some(package) = nodes
        .iter()
        .filter_map(NativeAcquiredOccurrence::package_grant)
        .find(|grant| grant.product().code().globals().is_empty())
    {
        coverage.package_leaf = true;
        let mut forged = actual.recorded_facts().as_ref().clone();
        let receipt = forged
            .package_receipts
            .iter_mut()
            .find(|receipt| receipt.stage_id == *package.stage_id())
            .unwrap();
        let mut facts = receipt.as_ref().clone();
        facts
            .occurrences
            .iter_mut()
            .find(|row| row.occurrence == package.index())
            .unwrap()
            .children
            .push(package.index());
        *receipt = Arc::new(facts);
        refuse_record(forged);
    }
    coverage
}

/// Count actual newly prepared targets that borrowed a package-bearing HOME
/// witness from a different current compiler cohort. The caller must observe
/// the incoming stage ID from its actual compiler receipt, never infer it from
/// already selected grants.
pub(crate) fn assert_borrowed_cohort_witnesses(
    prior: &LinkedNativeTarget,
    incoming: &LinkedNativeTarget,
    incoming_manifest_stage: [u8; 32],
) -> usize {
    let mut witnessed = 0;
    for selected in incoming.acquired() {
        let NativeOccurrenceId::Home(binding) = selected.occurrence() else {
            continue;
        };
        let Some(held) = prior.closure().home(binding) else {
            continue;
        };
        if held.code() != selected.code() {
            continue;
        }
        let original = held.selected_occurrences();
        if !original.iter().any(|node| {
            matches!(node.occurrence(),
            NativeOccurrenceId::Package {stage_id,..} if stage_id != incoming_manifest_stage)
        }) {
            continue;
        }
        for node in original {
            let borrowed = &incoming.closure().occurrences[&node.occurrence()];
            assert!(
                borrowed.same_linkage(&node),
                "borrowing preserves code and ordered exact slots"
            );
            if let Some(old) = node.home_grant() {
                let new = borrowed.home_grant().expect("borrowed HOME grant kind");
                assert_eq!(old.binding(), new.binding());
                assert!(old.product().same_issuance(new.product()));
            } else {
                let old = node.package_grant().expect("actual package grant");
                let new = borrowed
                    .package_grant()
                    .expect("borrowed package grant kind");
                assert!(
                    Arc::ptr_eq(old.product(), new.product()),
                    "borrow the actual immutable package product owner"
                );
                assert!(
                    Arc::ptr_eq(&old.receipt, &new.receipt),
                    "preserve the original receipt rather than rebinding equal code"
                );
                assert!(
                    Arc::ptr_eq(&old.row, &new.row),
                    "preserve the actual acquired package row owner"
                );
                assert_eq!(old.index(), new.index());
            }
        }
        witnessed += 1;
    }
    witnessed
}
