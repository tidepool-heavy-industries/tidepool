// Component histories use the existing representation-model certificates and
// receipt fixtures. Genuine compiler issuance remains an integration gate.
mod program_source_support_history {
    use super::*;

    // The fixture originals issue roles before custody can add dependencies.
    fn original_offer(
        products: &[CertifiedRecoveryProduct],
    ) -> (ArtifactView, CompilerInputProjection) {
        let originals = products
            .iter()
            .map(|product| {
                Arc::new(
                    crate::artifact_inventory::ArtifactEntry::original(
                        product.module_interface().unwrap().producer_sha256(),
                        product.clone(),
                    )
                    .unwrap(),
                )
            })
            .collect::<Vec<_>>();
        let projection = CompilerInputProjection::from_issued_entries(&originals).unwrap();
        let custody = support_view(products);
        projection.validate(&custody).unwrap();
        (custody, projection)
    }

    fn source_selection(
        projection: &CompilerInputProjection,
        custody: &ArtifactView,
    ) -> crate::certified_products::CertifiedSourceSelection {
        crate::certified_products::CertifiedSourceSelection::from_compiler_projection(
            projection,
            &custody.metadata_snapshot(),
            &tidepool_repr::execution_schema::InventoryOperation::new(Default::default()),
        )
        .unwrap()
    }

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
        .original_execution_fixture(&artifacts)
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

    // Expected native choices come from the explicit fixture offers, independently
    // of request metadata, source receipt rows and retained custody enumeration.
    fn offered_owners(products: &[CertifiedRecoveryProduct]) -> BTreeSet<CachedHomeOwner> {
        products
            .iter()
            .map(|product| product.owner().clone())
            .collect()
    }

    fn assert_private_choices(
        request: &ExactCompilationRequest,
        expected: &BTreeSet<CachedHomeOwner>,
        public: &ExactDeclarationContext,
    ) {
        let inputs = request.compiler_inputs().unwrap();
        let actual = inputs
            .metadata
            .entries
            .values()
            .filter_map(|entry| match &entry.payload {
                ArtifactPayload::Original(product) => Some(product.owner().clone()),
                _ => None,
            })
            .collect::<BTreeSet<_>>();
        assert_eq!(&actual, expected);
        assert_eq!(inputs.declaration_semantic_sha256, public.semantic_sha256());
        assert!(public.compiler_original_products().unwrap().is_empty());
        assert!(public.lexical_graph().is_empty());
        assert!(request.program_source_lexical().is_empty());
    }

    #[test]
    fn private_original_choices_survive_sparse_outputs_publication_capture_and_retry() {
        let directory = tempfile::tempdir().unwrap();
        let baseline = (0..20)
            .map(|index| support_product(&format!("Private{index:02}")))
            .collect::<Vec<_>>();
        let alternate = crate::certified_products::tests::original_groups_fixture_with_interface(
            &baseline[0].owner().module,
            Vec::new(),
            2,
            &BTreeMap::new(),
            baseline[0].interface_bytes().to_vec(),
        );
        let alternate = crate::certified_products::tests::recovered_witness_fixtures(&[
            crate::certified_products::fixture_finalized_product(alternate, [2; 32]),
        ])
        .remove(0)
        .product;
        assert_eq!(baseline[0].module_interface(), alternate.module_interface());
        assert_ne!(baseline[0].owner(), alternate.owner());
        let custody = support_view(&baseline)
            .merge(&support_view(&[alternate.clone()]))
            .unwrap();
        let sparse = [
            support_product("SparseScaffold"),
            support_product_in_unit("main", "Tidepool.Internal.Resume"),
        ];
        let (sparse_view, sparse_projection) = original_offer(&sparse);
        assert_eq!(sparse.len(), 2);
        let sparse_selection = source_selection(&sparse_projection, &sparse_view);
        let sparse_input =
            OriginalCompilerInputs::from_selection(&sparse_selection, &sparse_view).unwrap();

        // Both exact native versions share the public interface, but each history
        // selects one immutable original and retains the other only as custody.
        for choose_alternate in [false, true] {
            let mut selected = baseline.clone();
            let excluded = if choose_alternate {
                selected[0] = alternate.clone();
                baseline[0].clone()
            } else {
                alternate.clone()
            };
            let (_, projection) = original_offer(&selected);
            let selection = source_selection(&projection, &custody);
            let input = OriginalCompilerInputs::from_selection(&selection, &custody).unwrap();
            let public = Arc::new(
                ExactDeclarationContext::new(&[], &[], vec![])
                    .unwrap()
                    .extend_interface_artifacts(&custody)
                    .unwrap(),
            );
            let before = public.semantic_sha256();
            let mut expected = offered_owners(&selected);
            assert_eq!(expected.len(), 20);
            let root = directory.path().join(format!("history-{choose_alternate}"));
            std::fs::create_dir_all(&root).unwrap();
            let original = program_request(&root, public.clone());
            let request = original
                .in_program_context_with_private_input(
                    &root.join("planned-input"),
                    public.clone(),
                    &input,
                )
                .unwrap();
            assert_private_choices(&request, &expected, &public);
            let captured = request.clone();

            // Sparse output rows publish only interface custody; the private
            // carrier grows independently of this persistent source projection.
            let mut publication = request.clone();
            let published = publication
                .admit_program_support_with_selection(
                    public.clone(),
                    &sparse_view,
                    &[],
                    None,
                    &sparse_selection,
                )
                .unwrap();
            assert_private_choices(&request, &expected, &public);
            let continued = publication
                .in_program_context_with_private_input(
                    &root.join("sparse-input"),
                    published.clone(),
                    &sparse_input,
                )
                .unwrap();
            expected.extend(offered_owners(&sparse));
            assert_eq!(expected.len(), 22);
            assert_private_choices(&continued, &expected, &published);
            assert_private_choices(&captured, &offered_owners(&selected), &public);
            assert_eq!(public.semantic_sha256(), before);

            let (wrong_view, wrong_projection) = original_offer(&[excluded]);
            let wrong_selection = source_selection(&wrong_projection, &wrong_view);
            let wrong_input =
                OriginalCompilerInputs::from_selection(&wrong_selection, &wrong_view).unwrap();
            let stable_roles = continued.compiler_inputs().unwrap().projection.roles();
            assert!(continued
                .in_program_context_with_private_input(
                    &root.join("wrong-input"),
                    published.clone(),
                    &wrong_input,
                )
                .is_err());
            assert!(OriginalCompilerInputs::from_selection(&selection, &wrong_view).is_err());
            let without_first = support_view(&selected[1..]);
            assert!(OriginalCompilerInputs::from_selection(&selection, &without_first).is_err());
            assert_eq!(
                continued.compiler_inputs().unwrap().projection.roles(),
                stable_roles
            );
            assert_private_choices(&continued, &expected, &published);
            let retried = continued
                .in_program_context_with_private_input(
                    &root.join("repeated-input"),
                    published.clone(),
                    &input,
                )
                .unwrap();
            assert_private_choices(&retried, &expected, &published);

            // Omission is licensed only by the exact generated source owner.
            let receipt = import_receipt(&root, &continued, "Unadmitted");
            let mut value = read_receipt(&receipt);
            value.as_array_mut().unwrap()[8].as_array_mut().unwrap()[0]
                .as_array_mut()
                .unwrap()[3] = Value::Array(vec![]);
            write_receipt(&receipt, &value);
            let admission = continued
                .validate_receipt(&receipt, None, &published)
                .unwrap();
            let source = "module Consumer where\n";
            let generated = support_product("Consumer");
            let effective = continued.compiler_inputs().unwrap();
            let (generated_view, generated_projection) = original_offer(&[generated.clone()]);
            let full_view = effective.artifacts.merge(&generated_view).unwrap();
            let full_projection = effective.projection.merge(&generated_projection).unwrap();
            let full_selection = source_selection(&full_projection, &full_view);
            let full_input =
                OriginalCompilerInputs::from_selection(&full_selection, &full_view).unwrap();
            let support = full_input
                .for_program_continuation(
                    &effective.artifacts,
                    std::slice::from_ref(&admission),
                    source,
                )
                .unwrap();
            assert_eq!(support.projection, effective.projection);
            let selected_id =
                crate::artifact_inventory::ArtifactEntry::original([2; 32], selected[0].clone())
                    .unwrap()
                    .descriptor
                    .id;
            let missing_selected = full_view
                .select_roots(
                    full_view
                        .descriptors()
                        .into_iter()
                        .filter(|descriptor| descriptor.id != selected_id)
                        .map(|descriptor| descriptor.id)
                        .collect(),
                )
                .unwrap();
            assert!(full_input
                .for_program_continuation(
                    &missing_selected,
                    std::slice::from_ref(&admission),
                    source,
                )
                .is_err());
            let execution = ExactProductAdmission {
                request: &continued,
                source: &admission,
            }
            .original_execution_context(&full_input)
            .unwrap();
            let mut execution_expected = expected.clone();
            execution_expected.insert(generated.owner().clone());
            assert_eq!(
                offered_owners(&execution.compiler_original_products().unwrap()),
                execution_expected
            );
            assert_private_choices(&continued, &expected, &published);
        }
    }

    #[test]
    fn bounded_artifact_and_original_import_histories_match_set_oracle() {
        let directory = tempfile::tempdir().unwrap();
        let owners = ["A", "B", "C"].map(|module| identity("fixture", module));
        let unrelated = identity("fixture", "Unrelated");
        let products = ["A", "B", "C", "Unrelated"].map(support_product);
        let (all_artifacts, all_projection) = original_offer(&products);
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
                let (initial_artifacts, initial_projection) =
                    original_offer(&[products[first].clone()]);
                let initial = ProgramSourceSupport::extend(
                    None,
                    initial_artifacts,
                    initial_projection,
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
                    all_projection.clone(),
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
                let reordered = ProgramSourceSupport::extend(
                    Some(&current),
                    all_artifacts.clone(),
                    all_projection.clone(),
                    rows,
                )
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
                    all_projection.clone(),
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
                        all_projection.clone(),
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
                let before_projection = current.compiler_projection.clone();
                let (conflict, conflict_projection) =
                    original_offer(&[support_product_with_interface(
                        "fixture",
                        &first_owner.module,
                        b"replacement interface".to_vec(),
                    )]);
                assert!(request
                    .admit_program_support_with_selection(
                        context.clone(),
                        &conflict,
                        &[],
                        None,
                        &source_selection(&conflict_projection, &conflict),
                    )
                    .is_err());
                coverage.artifact_refusals += 1;
                let after = request.program_support.as_ref().unwrap();
                assert!(Arc::ptr_eq(&after.imports, &before_graph));
                assert_eq!(after.artifacts.descriptors(), before_artifacts);
                assert_eq!(after.compiler_projection, before_projection);
                assert_query(
                    directory.path(),
                    after,
                    &graph,
                    &all_owners,
                    &first_owner,
                    &mut coverage,
                );
                let retried = request
                    .admit_program_support_with_selection(
                        context.clone(),
                        &all_artifacts,
                        &[],
                        None,
                        &source_selection(&all_projection, &all_artifacts),
                    )
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
