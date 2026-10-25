// Component histories carry representation-model certificates through actual
// native and TYPE owners. SOURCE admission and generated-output omission require
// the actual compiler integration gate.
mod program_source_support_history {
    use super::*;

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
    fn private_original_choices_survive_sparse_type_growth_capture_and_retry() {
        let offer_operation = std::sync::Arc::new(
            tidepool_repr::execution_schema::InventoryOperation::new(Default::default()),
        );
        let mut offer_validation =
            crate::recovery_artifacts::PackageInterfaceValidation::with_inventory(offer_operation);
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
            crate::certified_products::fixture_finalized_product(
                alternate,
                program_test_producer_sha256(),
            ),
        ])
        .remove(0)
        .product;
        assert_eq!(baseline[0].module_interface(), alternate.module_interface());
        assert_ne!(baseline[0].owner(), alternate.owner());
        let custody = support_view(&baseline)
            .merge_fixture_selections(&support_view(&[alternate.clone()]))
            .unwrap();
        let sparse = support_product_in_unit("main", "CachedSparseHelper");
        let CompilerSupportFixture::Complete(sparse_inputs) =
            support_offer(std::slice::from_ref(&sparse))
        else {
            unreachable!("completed native fixture")
        };
        let sparse_support = sparse_inputs.compiler_source_support();
        assert!(!sparse_support.native_offers().is_empty());
        let sparse_input = OriginalCompilerInputs::from_issued_support(sparse_support);
        let sparse_types = sparse_support
            .selected_interfaces()
            .unwrap()
            .support_for_owners(vec![identity(&sparse.owner().unit, &sparse.owner().module)])
            .unwrap();

        for choose_alternate in [false, true] {
            let mut selected = baseline.clone();
            let excluded = if choose_alternate {
                selected[0] = alternate.clone();
                baseline[0].clone()
            } else {
                alternate.clone()
            };
            let CompilerSupportFixture::Complete(selected_inputs) = support_offer(&selected) else {
                unreachable!("completed native fixture")
            };
            let issued = selected_inputs
                .compiler_source_support()
                .retain_custody(&custody)
                .unwrap();
            assert!(!issued.native_offers().is_empty());
            let projection = issued.projection().clone();
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
            let empty = Arc::new(
                ExactDeclarationContext::from_authenticated_interfaces(
                    program_test_producer_sha256(),
                    &ArtifactInventory::default().empty_view(),
                    CompilerInputProjection::default(),
                )
                .unwrap(),
            );
            let original = empty
                .prepare_compilation(&root.join("request"), PROGRAM_TEST_PRODUCER)
                .unwrap()
                .program_admission()
                .in_program_context(public.clone())
                .unwrap();
            assert_eq!(original.compiler_inputs().metadata.entries.len(), 20);
            assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1);
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

            // Public TYPE growth and private Original choice use their actual
            // completed owners independently. Neither issues SOURCE authority.
            let published = Arc::new(
                public
                    .as_ref()
                    .clone()
                    .extend_issued_compiler_support(&sparse_types)
                    .unwrap(),
            );
            let publication = request.in_program_context(published.clone()).unwrap();
            assert_private_choices(&publication, &expected, &published);
            let before_continuation = REQUEST_COMPILER_INPUT_COMPUTATIONS.with(|count| count.get());
            let continued = publication
                .in_program_context_with_private_input(published.clone(), &sparse_input)
                .unwrap();
            expected.insert(sparse.owner().clone());
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
            assert_ne!(published.semantic_sha256(), before);
            let held = continued
                .compiler_inputs()
                .compiler_source_support()
                .native_offers();
            for offer in issued
                .native_offers()
                .iter()
                .chain(sparse_support.native_offers())
            {
                assert!(
                    held.iter().any(|actual| actual.same_issuance(offer)),
                    "growth retains the exact issuing native contribution"
                );
            }

            let CompilerSupportFixture::Complete(wrong_inputs) =
                support_offer(std::slice::from_ref(&excluded))
            else {
                unreachable!("completed alternate native fixture")
            };
            let wrong_input =
                OriginalCompilerInputs::from_issued_support(wrong_inputs.compiler_source_support());
            let independent = original
                .in_program_context_with_private_input(public.clone(), &wrong_input)
                .unwrap();
            assert_private_choices(
                &independent,
                &BTreeSet::from([excluded.owner().clone()]),
                &public,
            );
            let stable_inputs = Arc::clone(&continued.inputs.compiler);
            let stable_roles = continued.compiler_inputs().projection().roles();
            assert!(continued
                .in_program_context_with_private_input(published.clone(), &wrong_input)
                .is_err());
            let wrong_view = wrong_inputs.compiler_source_support().artifacts().clone();
            assert!(recover_fixture_compiler_support(
                projection.clone(),
                wrong_view,
                [],
                &mut offer_validation
            )
            .is_err());
            let without_first = support_view(&selected[1..]);
            assert!(recover_fixture_compiler_support(
                projection.clone(),
                without_first,
                [],
                &mut offer_validation
            )
            .is_err());
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
