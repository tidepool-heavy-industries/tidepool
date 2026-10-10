use super::*;

fn node(id: u8, scope: u8) -> ArtifactBindingNode {
    ArtifactBindingNode::Artifact(ArtifactBinding {
        artifact: ArtifactId([id; 32]),
        selection: ArtifactSelectionId([scope; 32]),
    })
}

fn rows(sources: &[(ArtifactBindingNode, Vec<ArtifactBindingNode>)]) -> BindingRows {
    sources
        .iter()
        .map(|(source, targets)| {
            (
                *source,
                targets
                    .iter()
                    .map(|target| (*target, ArtifactDependency::Interface))
                    .collect(),
            )
        })
        .collect()
}

#[test]
fn scope_identity_hashes_only_own_complete_rows_and_typed_references() {
    let provisional = ArtifactSelectionId([0; 32]);
    let own = node(1, 0);
    let external = node(2, 9);
    let base = rows(&[(own, vec![external]), (external, vec![])]);
    let issued = ScopeProofV3::issue(&base, provisional).unwrap();
    let mut expanded = base.clone();
    expanded.insert(
        node(3, 7),
        BTreeSet::from([(external, ArtifactDependency::Interface)]),
    );
    assert_eq!(ScopeProofV3::issue(&expanded, provisional).unwrap(), issued);
    let changed_external = rows(&[(own, vec![node(2, 8)]), (node(2, 8), vec![])]);
    assert_ne!(
        ScopeProofV3::issue(&changed_external, provisional)
            .unwrap()
            .selection,
        issued.selection
    );
    let with_zero = rows(&[(own, vec![external]), (node(4, 0), vec![])]);
    assert_ne!(
        ScopeProofV3::issue(&with_zero, provisional)
            .unwrap()
            .selection,
        issued.selection
    );
    let mut changed_label = base.clone();
    changed_label.insert(
        own,
        BTreeSet::from([(
            external,
            ArtifactDependency::NativeGroup {
                dependent_ordinal: 3,
                required_ordinal: 7,
            },
        )]),
    );
    assert_ne!(
        ScopeProofV3::issue(&changed_label, provisional)
            .unwrap()
            .selection,
        issued.selection
    );
    assert_eq!(issued.nodes.len(), 1);
    assert_eq!(issued.dependencies.len(), 1);
    assert!(ScopeProofV3::issue(&rows(&[(external, vec![])]), provisional).is_none());
}

#[test]
fn same_scope_cycles_are_valid_and_group_scope_does_not_create_a_carrier() {
    let provisional = ArtifactSelectionId([0; 32]);
    let issued = ScopeProofV3::issue(
        &rows(&[
            (node(1, 0), vec![node(2, 0)]),
            (node(2, 0), vec![node(1, 0)]),
        ]),
        provisional,
    )
    .unwrap();
    let selected = issued.authenticated_rows().unwrap();
    authenticate(&[Arc::new(issued.clone())], &selected).unwrap();
    let mut missing_zero = issued.clone();
    missing_zero.nodes.pop();
    assert!(missing_zero.authenticated_rows().is_err());
    let carrier = ScopeProofV3::issue(&rows(&[(node(5, 0), vec![])]), provisional).unwrap();
    let carrier_node = carrier.nodes[0];
    let group = ArtifactBindingNode::Group(NativeGroupBinding {
        binding: ArtifactBinding {
            artifact: ArtifactId([5; 32]),
            selection: provisional,
        },
        original_ordinal: 11,
    });
    let group = ScopeProofV3::issue(&rows(&[(group, vec![carrier_node])]), provisional).unwrap();
    assert_eq!(group.nodes.len(), 1);
    assert!(matches!(group.nodes[0], ArtifactBindingNode::Group(_)));
    assert_ne!(group.selection, carrier.selection);
    let mut selected = group.authenticated_rows().unwrap();
    selected.extend(carrier.authenticated_rows().unwrap());
    authenticate(&[Arc::new(carrier), Arc::new(group)], &selected).unwrap();
}

#[test]
fn proof_only_ancestors_survive_retirement_without_payload_authority() {
    use super::super::tests::entry;
    let inventory = ArtifactInventory::default();
    let ancestor = entry("Ancestor", &[]);
    let ancestor_id = ancestor.descriptor.id;
    let ancestor_view = inventory
        .admit(&inventory.empty_view(), vec![ancestor])
        .unwrap();
    let selected_entry = entry("Selected", &[]);
    let selected_id = selected_entry.descriptor.id;
    let issuing = inventory
        .admit(
            &ancestor_view,
            vec![selected_entry, entry("Witness", &["Ancestor"])],
        )
        .unwrap();
    let selected = issuing.select_roots(vec![selected_id]).unwrap();
    drop(issuing);
    drop(ancestor_view);
    assert_eq!(selected.artifact_ids(), vec![selected_id]);
    let graph = selected.capture_graph_selection();
    assert_eq!(graph.selections.len(), 2);
    let state = inventory.0.lock().unwrap();
    assert!(!state.payloads.contains_key(&ancestor_id));
    assert_eq!(state.selection_plans.len(), 2);
    assert_eq!(state.scope_dependents.values().sum::<usize>(), 1);
    drop(state);
    let reopened_inventory = ArtifactInventory::default();
    let reopened = reopened_inventory
        .recover_selection(
            &reopened_inventory.empty_view(),
            selected.entries(),
            &graph,
            &selected.binding_dependencies(),
        )
        .unwrap();
    assert_eq!(reopened.capture_graph_selection(), graph);
    assert_eq!(reopened.artifact_ids(), vec![selected_id]);
    drop(selected);
    assert!(inventory.0.lock().unwrap().selection_plans.is_empty());
    assert!(inventory.0.lock().unwrap().scope_dependents.is_empty());
    drop(reopened);
    assert!(reopened_inventory
        .0
        .lock()
        .unwrap()
        .selection_plans
        .is_empty());
}

#[test]
fn unchanged_bindings_issue_no_scope_and_empty_selection_stays_empty() {
    use super::super::tests::entry;
    let inventory = ArtifactInventory::default();
    let payload = entry("Root", &[]);
    let initial = inventory
        .admit(&inventory.empty_view(), vec![payload.clone()])
        .unwrap();
    let unchanged = inventory.admit(&initial, vec![payload]).unwrap();
    assert_eq!(
        unchanged.capture_graph_selection().selections,
        initial.capture_graph_selection().selections
    );
    assert_eq!(inventory.0.lock().unwrap().selection_plans.len(), 1);
    let empty = unchanged.select_roots(vec![]).unwrap();
    assert!(empty.capture_graph_selection().selections.is_empty());
    assert!(empty.capture_graph_selection().bindings.is_empty());
    assert!(empty.capture_graph_selection().roots.is_empty());
    let reopened_inventory = ArtifactInventory::default();
    let reopened = reopened_inventory
        .recover_selection(
            &reopened_inventory.empty_view(),
            empty.entries(),
            &empty.capture_graph_selection(),
            &[],
        )
        .unwrap();
    assert!(reopened.is_empty());
}

proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(64))]
    #[test]
    fn generated_admission_history_matches_independent_cohort_model(
        widths in proptest::collection::vec(1usize..7, 1..16),
        repetitions in proptest::collection::vec(proptest::bool::ANY, 1..16),
    ) {
        use super::super::tests::entry;
        let inventory = ArtifactInventory::default();
        let mut view = inventory.empty_view();
        let mut expected = BTreeMap::new();
        let mut total = 0;
        for (generation, width) in widths.iter().copied().enumerate() {
            let payloads = (0..width).map(|index| entry(&format!("History{generation}_{index}"), &[])).collect::<Vec<_>>();
            let own_ids = payloads.iter().map(|entry| entry.descriptor.id).collect::<BTreeSet<_>>();
            view = inventory.admit(&view, payloads.clone()).unwrap();
            let graph = view.capture_graph_selection();
            let scope = graph.bindings.iter().find(|binding| own_ids.contains(&binding.artifact)).unwrap().selection;
            expected.insert(scope, own_ids);
            total += width;
            if repetitions[generation % repetitions.len()] {
                view = inventory.admit(&view, payloads).unwrap();
            }
            let actual = view.capture_graph_selection().selections.into_iter().map(|proof| {
                proptest::prop_assert!(proof.dependencies.is_empty());
                Ok((proof.selection, proof.nodes.iter().map(|node| node.artifact()).collect::<BTreeSet<_>>()))
            }).collect::<Result<BTreeMap<_,_>, proptest::test_runner::TestCaseError>>()?;
            proptest::prop_assert_eq!(&actual, &expected);
            proptest::prop_assert_eq!(actual.values().map(BTreeSet::len).sum::<usize>(), total);
            let restored_inventory = ArtifactInventory::default();
            let restored = restored_inventory.recover_selection(&restored_inventory.empty_view(), view.entries(), &view.capture_graph_selection(), &[]).unwrap();
            proptest::prop_assert_eq!(restored.capture_graph_selection(), view.capture_graph_selection());
        }
    }
}
