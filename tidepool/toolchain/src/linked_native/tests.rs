//! Authentication controls entered only with actual compiler-issued targets.

use super::*;
use crate::recovery_artifacts::PackageInterfaceValidation;
use tidepool_repr::native_code::NativeCodeComponentFacts;

pub(crate) fn assert_actual_target_authentication(target: &LinkedNativeTarget) {
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
    let reissued = AcquiredNativeClosure::issue(
        nodes.clone(),
        actual.artifacts().inventory().empty_view(),
        &operation,
    )
    .expect("actual acquired rows pass the same live/restore gate");
    assert_eq!(reissued.identity(), actual.identity());

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
        if let NativeCodeGrant::Home { current, .. } = selected.grant() {
            assert!(
                current.is_none(),
                "restore cannot manufacture receiving-target Current correlation"
            );
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

    if let Some(home) = nodes
        .iter()
        .find(|node| matches!(node.acquisition, NativeAcquisition::Home { .. }))
    {
        // Corrupt DATA while preserving the original acquired product and body.
        // An equal executable body cannot authorize another issuing scope.
        let mut forged = nodes.clone();
        let node = forged
            .iter_mut()
            .find(|node| node.occurrence == home.occurrence)
            .unwrap();
        let NativeOccurrenceId::Home(binding) = node.occurrence else {
            unreachable!()
        };
        let mut data = serde_json::to_value(binding).unwrap();
        let byte = data["binding"]["selection"][0].as_u64().unwrap();
        data["binding"]["selection"][0] = serde_json::Value::from(byte ^ 1);
        node.occurrence = NativeOccurrenceId::Home(serde_json::from_value(data).unwrap());
        assert!(
            AcquiredNativeClosure::issue(
                forged,
                actual.artifacts().inventory().empty_view(),
                &operation
            )
            .is_err(),
            "foreign scope with identical product/body must refuse at the acquisition issuer"
        );
    }
    for selector_kind in [false, true] {
        let slot = nodes.iter().enumerate().find_map(|(node_index, node)| {
            if !matches!(node.acquisition, NativeAcquisition::Home { .. }) {
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
        let mut forged = nodes.clone();
        let mut selectors = forged[node_index].imports.to_vec();
        match &mut selectors[slot] {
            NativeGrantSelector::Code(NativeOccurrenceId::Home(binding)) => {
                binding.original_ordinal ^= 1
            }
            NativeGrantSelector::Retained { binding } => binding.selection.0[0] ^= 1,
            _ => continue,
        }
        forged[node_index].imports = selectors.into();
        assert!(
            AcquiredNativeClosure::issue(
                forged,
                actual.artifacts().inventory().empty_view(),
                &operation
            )
            .is_err(),
            "an exact HOME source or retained selector cannot be rebound through equal code"
        );
    }
    if let Some(index) = nodes.iter().position(|node| {
        matches!(&node.acquisition,
        NativeAcquisition::Package{product,..} if product.code().globals().is_empty())
    }) {
        let mut forged = nodes.clone();
        let node = &mut forged[index];
        let NativeAcquisition::Package {
            receipt,
            occurrence,
            ..
        } = &mut node.acquisition
        else {
            unreachable!()
        };
        let mut facts = receipt.facts.as_ref().clone();
        facts
            .occurrences
            .iter_mut()
            .find(|row| row.occurrence == *occurrence)
            .unwrap()
            .children
            .push(*occurrence);
        *receipt = NativePackageReceipt::from_facts(Arc::new(facts), None, &operation).unwrap();
        assert!(
            AcquiredNativeClosure::issue(
                forged,
                actual.artifacts().inventory().empty_view(),
                &operation
            )
            .is_err(),
            "a leaf's forged receipt child must refuse even though no import loop executes"
        );
    }
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
            matches!(node.occurrence,
            NativeOccurrenceId::Package {stage_id,..} if stage_id != incoming_manifest_stage)
        }) {
            continue;
        }
        for node in original {
            let borrowed = &incoming.closure().occurrences[&node.occurrence];
            assert_eq!(
                borrowed.code, node.code,
                "borrowing cannot change a historical code choice"
            );
            assert_eq!(
                borrowed.imports, node.imports,
                "borrowing preserves ordered exact selectors"
            );
            match (&node.acquisition, &borrowed.acquisition) {
                (
                    NativeAcquisition::Home { grant: old },
                    NativeAcquisition::Home { grant: new },
                ) => {
                    assert_eq!(old.binding(), new.binding());
                    assert!(old.product().same_issuance(new.product()));
                }
                (
                    NativeAcquisition::Package {
                        product: old,
                        receipt: old_receipt,
                        occurrence: old_occ,
                    },
                    NativeAcquisition::Package {
                        product: new,
                        receipt: new_receipt,
                        occurrence: new_occ,
                    },
                ) => {
                    assert!(
                        Arc::ptr_eq(old, new),
                        "borrow the actual immutable package product owner"
                    );
                    assert!(
                        Arc::ptr_eq(old_receipt, new_receipt),
                        "preserve the original receipt rather than rebinding the same code"
                    );
                    assert_eq!(old_occ, new_occ);
                }
                _ => panic!("borrowed acquired witness changed grant kind"),
            }
        }
        witnessed += 1;
    }
    witnessed
}
