// Component histories use the existing representation-model certificates and
// receipt fixtures. Genuine compiler issuance remains an integration gate.
mod program_source_support_history {
    use super::*;

    type Graph = BTreeMap<ExactModuleIdentity, BTreeSet<ExactModuleIdentity>>;

    #[derive(Default, Debug)]
    struct Coverage {
        histories: usize,
        queries: usize,
        extensions: usize,
        graph_refusals: usize,
        artifact_refusals: usize,
        missing_rows: usize,
        missing_queries: usize,
        reached_missing_frontiers: usize,
        unreachable_deletions: usize,
        blocked_downstream_queries: usize,
        repairs: usize,
        captured_queries: usize,
        cyclic_histories: usize,
        shared_dependency_histories: usize,
    }

    // Finite-set saturation is independent of the production stack traversal.
    // An absent adjacency row is an unknown frontier, never an empty leaf.
    fn reachable(graph: &Graph, root: &ExactModuleIdentity) -> BTreeSet<ExactModuleIdentity> {
        let mut selected = BTreeSet::from([root.clone()]);
        loop {
            let next = selected
                .iter()
                .flat_map(|owner| graph.get(owner).into_iter().flatten())
                .cloned()
                .chain(selected.iter().cloned())
                .collect::<BTreeSet<_>>();
            if next == selected {
                return selected;
            }
            selected = next;
        }
    }

    fn assert_query(
        root: &Path,
        support: &ProgramSourceSupport,
        graph: &Graph,
        owners: &BTreeSet<ExactModuleIdentity>,
        selected: &ExactModuleIdentity,
        coverage: &mut Coverage,
    ) {
        let empty = Arc::new(ExactDeclarationContext::new(&[], &[], vec![]).unwrap());
        let mut request = program_request(root, empty.clone());
        request.program_support = Some(support.clone());
        let receipt = import_receipt_owner(
            root,
            &request,
            &selected.unit,
            &selected.module,
            "none",
            false,
        );
        let admission = request.validate_receipt(&receipt, None, &empty).unwrap();
        let artifacts = support
            .artifacts
            .merge(&support_view(&[support_product("Consumer")]))
            .unwrap();
        let actual = ExactProductAdmission {
            request: &request,
            source: &admission,
        }
        .original_execution_context(&artifacts)
        .unwrap();

        let reached = reachable(graph, selected);
        let known = reached
            .iter()
            .filter(|owner| owners.contains(*owner) && graph.contains_key(*owner))
            .cloned()
            .collect::<BTreeSet<_>>();
        let missing = reached.difference(&known).cloned().collect::<Vec<_>>();
        let consumer = identity("fixture", "Consumer");
        let expected = known
            .iter()
            .map(|owner| {
                (
                    owner.clone(),
                    graph[owner].intersection(&known).cloned().collect(),
                )
            })
            .chain(std::iter::once((
                consumer.clone(),
                known
                    .contains(selected)
                    .then(|| selected.clone())
                    .into_iter()
                    .collect(),
            )))
            .collect::<Graph>();
        let actual_graph = actual
            .lexical_graph()
            .iter()
            .map(|node| (node.owner.clone(), node.imports.iter().cloned().collect()))
            .collect::<Graph>();
        assert_eq!(actual_graph, expected);
        if missing.is_empty() {
            assert_eq!(
                actual.original_instance_environment(),
                &OriginalInstanceEnvironment::Complete { target: consumer }
            );
        } else {
            assert_eq!(
                actual.original_instance_environment(),
                &OriginalInstanceEnvironment::MissingOriginalOwners(missing)
            );
        }
        // Private execution evidence cannot change the request's public scope.
        assert!(request.context.lexical_graph().is_empty());
        assert!(request.program_source_lexical().is_empty());
        coverage.queries += 1;
    }

    #[test]
    fn bounded_artifact_and_original_import_histories_match_set_oracle() {
        let directory = tempfile::tempdir().unwrap();
        let owners = ["A", "B", "C"].map(|module| identity("fixture", module));
        let unrelated = identity("fixture", "Unrelated");
        let products = ["A", "B", "C", "Unrelated"].map(support_product);
        let all_artifacts = support_view(&products);
        // These interfaces intentionally have no nominal/type requirements.
        assert!(all_artifacts
            .entries()
            .iter()
            .all(|entry| entry.requirements.is_empty()));
        let all_owners = owners
            .iter()
            .cloned()
            .chain([unrelated.clone()])
            .collect::<BTreeSet<_>>();
        let mut coverage = Coverage::default();
        for mask in 0..64u8 {
            let mut graph = all_owners
                .iter()
                .cloned()
                .map(|owner| (owner, BTreeSet::new()))
                .collect::<Graph>();
            let mut bit = 0;
            for from in &owners {
                for to in &owners {
                    if from != to {
                        if mask & (1 << bit) != 0 {
                            graph.get_mut(from).unwrap().insert(to.clone());
                        }
                        bit += 1;
                    }
                }
            }
            for first in 0..3 {
                coverage.histories += 1;
                coverage.cyclic_histories += usize::from(owners.iter().any(|owner| {
                    graph[owner]
                        .iter()
                        .any(|next| reachable(&graph, next).contains(owner))
                }));
                coverage.shared_dependency_histories +=
                    usize::from(owners.iter().any(|owner| {
                        graph.values().filter(|edges| edges.contains(owner)).count() > 1
                    }));
                let first_owner = owners[first].clone();
                let initial_graph =
                    Graph::from([(first_owner.clone(), graph[&first_owner].clone())]);
                let initial_owners = BTreeSet::from([first_owner.clone()]);
                let initial = ProgramSourceSupport::extend(
                    None,
                    support_view(&[products[first].clone()]),
                    [(
                        first_owner.clone(),
                        graph[&first_owner].iter().cloned().collect(),
                    )],
                )
                .unwrap();
                coverage.extensions += 1;
                let captured = initial.clone();
                assert_query(
                    directory.path(),
                    &initial,
                    &initial_graph,
                    &initial_owners,
                    &first_owner,
                    &mut coverage,
                );

                let mut rows = graph
                    .iter()
                    .map(|(owner, edges)| {
                        (owner.clone(), edges.iter().cloned().collect::<Vec<_>>())
                    })
                    .collect::<Vec<_>>();
                rows.rotate_left(first);
                if mask & 1 != 0 {
                    rows.reverse();
                }
                let current = ProgramSourceSupport::extend(
                    Some(&initial),
                    all_artifacts.clone(),
                    rows.clone(),
                )
                .unwrap();
                coverage.extensions += 1;
                assert_eq!(
                    current.imports.as_ref(),
                    &graph
                        .iter()
                        .map(|(owner, edges)| (owner.clone(), edges.iter().cloned().collect()))
                        .collect()
                );
                for selected in &owners {
                    assert_query(
                        directory.path(),
                        &current,
                        &graph,
                        &all_owners,
                        selected,
                        &mut coverage,
                    );
                }
                for (_, edges) in &mut rows {
                    edges.reverse();
                    edges.extend(edges.clone());
                }
                rows.reverse();
                let reordered =
                    ProgramSourceSupport::extend(Some(&current), all_artifacts.clone(), rows)
                        .unwrap();
                coverage.extensions += 1;
                assert_eq!(reordered.imports, current.imports);
                assert_query(
                    directory.path(),
                    &reordered,
                    &graph,
                    &all_owners,
                    &first_owner,
                    &mut coverage,
                );
                assert_query(
                    directory.path(),
                    &captured,
                    &initial_graph,
                    &initial_owners,
                    &first_owner,
                    &mut coverage,
                );
                coverage.captured_queries += 1;

                let mut changed = graph[&first_owner].iter().cloned().collect::<Vec<_>>();
                changed.push(unrelated.clone());
                assert!(ProgramSourceSupport::extend(
                    Some(&current),
                    all_artifacts.clone(),
                    [(first_owner.clone(), changed)]
                )
                .is_err());
                coverage.graph_refusals += 1;
                assert_query(
                    directory.path(),
                    &current,
                    &graph,
                    &all_owners,
                    &first_owner,
                    &mut coverage,
                );

                for omitted in &owners {
                    let mut incomplete = current.clone();
                    Arc::make_mut(&mut incomplete.imports).remove(omitted);
                    let mut incomplete_graph = graph.clone();
                    incomplete_graph.remove(omitted);
                    for selected in &owners {
                        assert_query(
                            directory.path(),
                            &incomplete,
                            &incomplete_graph,
                            &all_owners,
                            selected,
                            &mut coverage,
                        );
                        coverage.missing_queries += 1;
                        if reachable(&incomplete_graph, selected).contains(omitted) {
                            coverage.reached_missing_frontiers += 1;
                        } else {
                            coverage.unreachable_deletions += 1;
                        }
                        coverage.blocked_downstream_queries += usize::from(
                            !reachable(&graph, selected)
                                .is_subset(&reachable(&incomplete_graph, selected)),
                        );
                    }
                    coverage.missing_rows += 1;
                    let repaired = ProgramSourceSupport::extend(
                        Some(&incomplete),
                        all_artifacts.clone(),
                        [(omitted.clone(), graph[omitted].iter().cloned().collect())],
                    )
                    .unwrap();
                    coverage.extensions += 1;
                    assert_query(
                        directory.path(),
                        &repaired,
                        &graph,
                        &all_owners,
                        omitted,
                        &mut coverage,
                    );
                    coverage.repairs += 1;
                    assert!(!incomplete.imports.contains_key(omitted));
                }

                let context = Arc::new(
                    ExactDeclarationContext::new(&[], &[], vec![])
                        .unwrap()
                        .extend_checked_original_products([2; 32], &products)
                        .unwrap(),
                );
                let mut request = program_request(directory.path(), context.clone());
                request.program_support = Some(current.clone());
                let before_graph = Arc::clone(&current.imports);
                let before_artifacts = current.artifacts.descriptors();
                let conflict = support_view(&[support_product_with_interface(
                    "fixture",
                    &first_owner.module,
                    b"replacement interface".to_vec(),
                )]);
                assert!(request
                    .admit_program_support(context.clone(), &conflict, &[], None)
                    .is_err());
                coverage.artifact_refusals += 1;
                let after = request.program_support.as_ref().unwrap();
                assert!(Arc::ptr_eq(&after.imports, &before_graph));
                assert_eq!(after.artifacts.descriptors(), before_artifacts);
                assert_query(
                    directory.path(),
                    after,
                    &graph,
                    &all_owners,
                    &first_owner,
                    &mut coverage,
                );
                let retried = request
                    .admit_program_support(context.clone(), &all_artifacts, &[], None)
                    .unwrap();
                coverage.extensions += 1;
                assert_eq!(retried.as_ref(), context.as_ref());
                assert!(retried.lexical_graph().is_empty());
                assert_query(
                    directory.path(),
                    request.program_support.as_ref().unwrap(),
                    &graph,
                    &all_owners,
                    &first_owner,
                    &mut coverage,
                );
            }
        }
        assert_eq!(coverage.histories, 192);
        assert_eq!(coverage.queries, 4032);
        assert_eq!(coverage.extensions, 1344);
        assert_eq!(coverage.graph_refusals, 192);
        assert_eq!(coverage.artifact_refusals, 192);
        assert_eq!(coverage.missing_rows, 576);
        assert_eq!(coverage.missing_queries, 1728);
        assert_eq!(coverage.reached_missing_frontiers, 1296);
        assert_eq!(coverage.unreachable_deletions, 432);
        assert_eq!(coverage.blocked_downstream_queries, 576);
        assert_eq!(coverage.repairs, 576);
        assert_eq!(coverage.captured_queries, 192);
        assert_eq!(coverage.cyclic_histories, 117);
        assert_eq!(coverage.shared_dependency_histories, 111);
        println!("original-import component coverage: {coverage:?}");
    }
}
