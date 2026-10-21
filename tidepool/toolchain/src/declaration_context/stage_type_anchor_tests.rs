//! Data-only mutations at the actual World stage projection boundary.

use super::*;

pub(super) fn assert_actual_type_anchors(
    continuation: &WorldProgramContinuation,
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
        continuation
            .inputs
            .source_support
            .project_world_stage_inputs(
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
        let selected = continuation
            .inputs
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
