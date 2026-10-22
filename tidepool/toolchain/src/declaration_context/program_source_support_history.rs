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

    type Graph = BTreeMap<ExactModuleIdentity, BTreeSet<ExactModuleIdentity>>;

    #[derive(Default, Debug)]
    struct ClosureCoverage {
        complete: usize,
        missing: usize,
        disconnected: usize,
        reordered_duplicates: usize,
        captured: usize,
        repaired: usize,
        cyclic: usize,
        shared_children: usize,
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

    // This reader consumes plain graph data. These checks issue no source
    // admission, compiler role, completed product or lexical authority.
    fn assert_closure_reader(
        graph: &Graph,
        rows: &BTreeMap<ExactModuleIdentity, Vec<ExactModuleIdentity>>,
        root: &ExactModuleIdentity,
        duplicate_root: bool,
        coverage: &mut ClosureCoverage,
    ) {
        let expected = reachable(graph, root);
        let complete = expected.iter().all(|owner| graph.contains_key(owner));
        let roots = if duplicate_root {
            vec![root.clone(), root.clone()]
        } else {
            vec![root.clone()]
        };
        let reads = std::cell::RefCell::new(BTreeMap::<ExactModuleIdentity, usize>::new());
        let actual = crate::declaration_join::keyed_source_lexical_closure(
            &roots,
            |owner| {
                *reads.borrow_mut().entry(owner.clone()).or_default() += 1;
                rows.get(owner).cloned()
            },
            |_| None,
        );
        if !complete {
            assert!(
                actual.is_err(),
                "a reached unknown row is not an empty leaf"
            );
            coverage.missing += 1;
            return;
        }
        let actual = actual.unwrap();
        let actual_graph = actual
            .lexical
            .iter()
            .map(|node| (node.owner.clone(), node.imports.iter().cloned().collect()))
            .collect::<Graph>();
        let expected_graph = graph
            .iter()
            .filter(|(owner, _)| expected.contains(*owner))
            .map(|(owner, edges)| (owner.clone(), edges.clone()))
            .collect::<Graph>();
        assert_eq!(actual_graph, expected_graph);
        assert_eq!(
            actual.roots.into_iter().collect::<BTreeSet<_>>(),
            BTreeSet::from([root.clone()])
        );
        let reads = reads.into_inner();
        assert_eq!(reads.keys().cloned().collect::<BTreeSet<_>>(), expected);
        assert!(
            reads.values().all(|count| *count == 1),
            "cycles and repeated edges visit each reached row once"
        );
        coverage.complete += 1;
    }

    fn assert_retained_input_reads(request: &ProgramAdmissionContinuation) {
        let before = REQUEST_COMPILER_INPUT_COMPUTATIONS.with(|count| count.get());
        let inputs = request.compiler_inputs();
        let captured = request.clone();
        for _ in 0..16 {
            assert!(std::ptr::eq(inputs, request.compiler_inputs()));
            assert!(std::ptr::eq(inputs, captured.compiler_inputs()));
        }
        assert_eq!(
            REQUEST_COMPILER_INPUT_COMPUTATIONS.with(|count| count.get()),
            before,
            "reads and captures must retain the issued input summary"
        );
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
        request: &ProgramAdmissionContinuation,
        expected: &BTreeSet<CachedHomeOwner>,
        public: &ExactDeclarationContext,
    ) {
        assert_retained_input_reads(request);
        let inputs = request.compiler_inputs();
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
        assert!(Arc::ptr_eq(&request.context().owner, &public.owner));
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
            .merge_fixture_selections(&support_view(&[alternate.clone()]))
            .unwrap();
        let sparse = [
            support_product("Consumer"),
            support_product_in_unit("main", "CachedSparseHelper"),
        ];
        let (sparse_view, sparse_projection) = original_offer(&sparse);
        assert_eq!(sparse.len(), 2);

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
            let (selected_view, projection) = original_offer(&selected);
            let issued = recover_fixture_compiler_support(projection.clone(), selected_view, [])
                .unwrap()
                .retain_custody(&custody)
                .unwrap();
            let input = OriginalCompilerInputs::from_issued_support(&issued);
            let types = issued
                .selected_interfaces()
                .unwrap()
                .support_for_owners(projection.snapshot_roles().keys().cloned().collect())
                .unwrap()
                .retain_custody(&custody)
                .unwrap();
            let public = Arc::new(
                ExactDeclarationContext::new(&[], &[], vec![])
                    .unwrap()
                    .extend_issued_compiler_support(&types)
                    .unwrap(),
            );
            let before = public.semantic_sha256();
            let mut expected = offered_owners(&selected);
            assert_eq!(expected.len(), 20);
            let root = directory.path().join(format!("history-{choose_alternate}"));
            std::fs::create_dir_all(&root).unwrap();
            // Public role growth and private promotion retain one semantic owner;
            // neither transition creates compiler transport copies.
            let empty = Arc::new(ExactDeclarationContext::new(&[], &[], vec![]).unwrap());
            let original = program_request(&root, empty)
                .program_admission()
                .in_program_context(public.clone())
                .unwrap();
            assert_eq!(original.compiler_inputs().metadata.entries.len(), 20);
            assert_eq!(
                std::fs::read_dir(&root).unwrap().count(),
                1,
                "semantic public roles must not create transport files"
            );
            let before_promotion = REQUEST_COMPILER_INPUT_COMPUTATIONS.with(|count| count.get());
            let request = original
                .in_program_context_with_private_input(public.clone(), &input)
                .unwrap();
            assert_eq!(
                REQUEST_COMPILER_INPUT_COMPUTATIONS.with(|count| count.get()),
                before_promotion + 1,
                "promotion computes and retains exactly one new input summary"
            );
            assert!(!std::ptr::eq(
                original.compiler_inputs(),
                request.compiler_inputs()
            ));
            assert_private_choices(&request, &expected, &public);
            assert_private_choices(&original, &BTreeSet::new(), &public);
            let captured = request.clone();

            let receipt = import_receipt(
                &root,
                request.receipt_admission().physical_request(),
                "Unadmitted",
            );
            let mut value = read_receipt(&receipt);
            value.as_array_mut().unwrap()[8].as_array_mut().unwrap()[0]
                .as_array_mut()
                .unwrap()[3] = Value::Array(vec![]);
            write_receipt(&receipt, &value);
            let mut validation = PackageInterfaceValidation::default();
            let admission = request
                .receipt_admission()
                .validate_receipt_with_validation(&receipt, None, &public, &mut validation)
                .unwrap();
            let generated_owner = admission.generated_source_owner();
            assert_eq!(generated_owner, identity("fixture", "Consumer"));
            let source = "module Consumer where\n";
            let effective = request.compiler_inputs();
            let full_view = effective
                .artifacts()
                .merge_fixture_selections(&sparse_view)
                .unwrap();
            let full_projection = effective.projection().merge(&sparse_projection).unwrap();
            let mut full_selection =
                crate::certified_products::CertifiedSourceSelection::from_compiler_projection(
                    &full_projection,
                    &full_view.metadata_snapshot(),
                    &validation.inventory,
                )
                .unwrap();
            let full_stage = CapturedCompilerStage::issue(
                &mut full_selection,
                &full_view,
                Vec::new(),
                &mut validation,
            )
            .unwrap();
            let full_input = full_stage.compiler_inputs();
            let full_selection = full_stage.source_selection();
            let execution = (&request)
                .receipt_admission()
                .product_admission(&admission)
                .original_execution_context(full_input)
                .unwrap();
            expected.extend(offered_owners(&sparse));
            assert_eq!(expected.len(), 22);
            assert_eq!(
                offered_owners(&execution.compiler_original_products().unwrap()),
                expected
            );

            // The sparse output adds its generated target and a cached helper. Only the
            // authenticated generated target leaves the continuation offer.
            let continuation_artifacts = full_view
                .select_roots(
                    full_view
                        .descriptors()
                        .into_iter()
                        .filter(|descriptor| descriptor.owner != generated_owner)
                        .map(|descriptor| descriptor.id)
                        .collect(),
                )
                .unwrap();
            let continuation_input = full_input
                .for_program_continuation(
                    &continuation_artifacts,
                    std::slice::from_ref(&admission),
                    source,
                )
                .unwrap();
            let mut publication = request.clone();
            let published = publication
                .admit_program_support_with_selection(
                    public.clone(),
                    &continuation_artifacts,
                    std::slice::from_ref(&admission),
                    None,
                    full_selection,
                )
                .unwrap();
            assert_private_choices(&request, &offered_owners(&selected), &public);
            let before_continuation = REQUEST_COMPILER_INPUT_COMPUTATIONS.with(|count| count.get());
            let continued = publication
                .in_program_context_with_private_input(published.clone(), &continuation_input)
                .unwrap();
            assert!(expected.remove(sparse[0].owner()));
            assert_eq!(expected.len(), 21);
            assert_eq!(
                REQUEST_COMPILER_INPUT_COMPUTATIONS.with(|count| count.get()),
                before_continuation + 1,
                "growth computes and retains exactly one new input summary"
            );
            assert!(!std::ptr::eq(
                request.compiler_inputs(),
                continued.compiler_inputs()
            ));
            assert_private_choices(&continued, &expected, &published);
            assert_private_choices(&captured, &offered_owners(&selected), &public);
            assert_eq!(public.semantic_sha256(), before);

            let (wrong_view, wrong_projection) = original_offer(&[excluded]);
            let wrong_support =
                recover_fixture_compiler_support(wrong_projection, wrong_view.clone(), []).unwrap();
            let wrong_input = OriginalCompilerInputs::from_issued_support(&wrong_support);
            let stable_inputs = Arc::clone(&continued.inputs.compiler);
            let stable_roles = continued.compiler_inputs().projection().roles();
            assert!(continued
                .in_program_context_with_private_input(published.clone(), &wrong_input)
                .is_err());
            assert!(recover_fixture_compiler_support(projection.clone(), wrong_view, []).is_err());
            let without_first = support_view(&selected[1..]);
            assert!(
                recover_fixture_compiler_support(projection.clone(), without_first, []).is_err()
            );
            assert_eq!(
                continued.compiler_inputs().projection().roles(),
                stable_roles
            );
            assert!(std::ptr::eq(
                stable_inputs.as_ref(),
                continued.compiler_inputs()
            ));
            assert_private_choices(&continued, &expected, &published);
            let retried = continued
                .in_program_context_with_private_input(published.clone(), &input)
                .unwrap();
            assert_private_choices(&retried, &expected, &published);

            // An inherited native carrier cannot borrow the generated omission.
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
            assert!(
                full_selection
                    .compiler_support_projection(
                        &missing_selected,
                        std::slice::from_ref(&admission)
                    )
                    .is_err(),
                "a source admission may omit only its generated target"
            );
            assert_private_choices(&continued, &expected, &published);
        }
    }

    #[test]
    fn bounded_source_closure_reader_histories_match_set_oracle() {
        let owners = ["A", "B", "C"].map(|module| identity("fixture", module));
        let unrelated = identity("fixture", "Unrelated");
        let all = owners
            .iter()
            .cloned()
            .chain([unrelated.clone()])
            .collect::<BTreeSet<_>>();
        let rows = |graph: &Graph| {
            graph
                .iter()
                .map(|(owner, edges)| (owner.clone(), edges.iter().cloned().collect::<Vec<_>>()))
                .collect::<BTreeMap<_, _>>()
        };
        let mut coverage = ClosureCoverage::default();
        // Every directed three-node graph without reflexive edges, including
        // diamonds and cycles. The saturation oracle does not use the reader.
        for mask in 0..64u8 {
            let mut graph = all
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
            for root in &owners {
                let captured = graph.clone();
                assert_closure_reader(&graph, &rows(&graph), root, false, &mut coverage);
                coverage.cyclic += usize::from(owners.iter().any(|owner| {
                    graph[owner]
                        .iter()
                        .any(|next| reachable(&graph, next).contains(owner))
                }));
                coverage.shared_children +=
                    usize::from(owners.iter().any(|owner| {
                        graph.values().filter(|edges| edges.contains(owner)).count() > 1
                    }));

                let mut reordered = rows(&graph);
                for edges in reordered.values_mut() {
                    edges.reverse();
                    edges.extend(edges.clone());
                }
                assert_closure_reader(&graph, &reordered, root, true, &mut coverage);
                coverage.reordered_duplicates += 1;

                let mut disconnected = graph.clone();
                disconnected.remove(&unrelated);
                assert_closure_reader(
                    &disconnected,
                    &rows(&disconnected),
                    root,
                    false,
                    &mut coverage,
                );
                coverage.disconnected += 1;

                for omitted in &owners {
                    let mut incomplete = graph.clone();
                    incomplete.remove(omitted);
                    assert_closure_reader(
                        &incomplete,
                        &rows(&incomplete),
                        root,
                        false,
                        &mut coverage,
                    );
                    let mut repaired = incomplete;
                    repaired.insert(omitted.clone(), graph[omitted].clone());
                    assert_closure_reader(&repaired, &rows(&repaired), root, false, &mut coverage);
                    coverage.repaired += 1;
                }

                let mut changed = graph.clone();
                changed.get_mut(root).unwrap().insert(unrelated.clone());
                assert!(reachable(&changed, root).contains(&unrelated));
                assert!(!reachable(&captured, root).contains(&unrelated));
                assert_closure_reader(&changed, &rows(&changed), root, false, &mut coverage);
                assert_closure_reader(&captured, &rows(&captured), root, false, &mut coverage);
                assert_eq!(captured, graph);
                coverage.captured += 1;
            }
        }
        assert_eq!(coverage.reordered_duplicates, 64 * 3);
        assert_eq!(coverage.disconnected, 64 * 3);
        assert_eq!(coverage.repaired, 64 * 3 * 3);
        assert_eq!(coverage.captured, 64 * 3);
        assert!(coverage.complete > 0 && coverage.missing > 0);
        assert!(coverage.cyclic > 0 && coverage.shared_children > 0);
        tracing::info!(?coverage, "pure source closure reader histories");
    }
}
