fn private_directory_census(root: &Path) -> (usize, u64) {
    let mut pending = vec![root.to_path_buf()];
    let mut files = 0;
    let mut bytes = 0;
    while let Some(path) = pending.pop() {
        let metadata = std::fs::symlink_metadata(&path).unwrap();
        assert!(!metadata.is_symlink());
        if metadata.is_dir() {
            pending.extend(
                std::fs::read_dir(path)
                    .unwrap()
                    .map(|entry| entry.unwrap().path()),
            );
        } else {
            assert!(metadata.is_file());
            files += 1;
            bytes += metadata.len();
        }
    }
    (files, bytes)
}

fn captured_materialization_context(
    source: &Arc<ExactDeclarationContext>,
    roots: Vec<ArtifactId>,
) -> Arc<ExactDeclarationContext> {
    let support = source
        .compiler_source_support()
        .select_support_roots(roots)
        .unwrap();
    let mut selected = source.as_ref().clone();
    selected = selected.rebuild(|facts| facts.compiler_support = support);
    {
        let updated = crate::exact_context::ExactLexicalSnapshot::default();
        selected = selected.rebuild(|facts| facts.lexical = updated);
    }
    selected.normalize().unwrap();
    Arc::new(selected)
}

fn check_materialized_capture_history(length: usize, first: usize, second: usize, warm: bool) {
    let (mut context, producer) = metadata_fixture();
    let scratch = tempfile::tempdir().unwrap();
    let base = context
        .prepare_compilation(&scratch.path().join("base"), &producer)
        .unwrap();
    let mut owners = vec![Arc::downgrade(base.materialization.as_ref().unwrap())];
    let mut directories = vec![base
        .materialization
        .as_ref()
        .unwrap()
        .directory()
        .path()
        .to_path_buf()];
    drop(base);
    let mut ids = Vec::new();
    for index in 0..length {
        let module = format!("CaptureHistory{index}");
        let product = crate::certified_products::fixture_finalized_product(
            support_product(&module),
            context.facts.producer,
        );
        context = Arc::new(
            context
                .as_ref()
                .clone()
                .extend_checked_original_products(context.facts.producer, &[product])
                .unwrap(),
        );
        ids.push(
            context.compiler_metadata_snapshot().unwrap().entries[&identity("fixture", &module)]
                .descriptor
                .id,
        );
        let request = context
            .prepare_compilation(&scratch.path().join(format!("item-{index}")), &producer)
            .unwrap();
        let owner = request.materialization.as_ref().unwrap();
        assert_eq!(
            owner.rows.len(),
            1,
            "growing contexts still store one new row"
        );
        assert_eq!(owner._parents.len(), 1, "full-context deltas remain shared");
        owners.push(Arc::downgrade(owner));
        directories.push(owner.directory().path().to_path_buf());
    }
    let before = directories
        .iter()
        .map(|root| private_directory_census(root))
        .fold((0, 0), |left, right| (left.0 + right.0, left.1 + right.1));
    let history_rows = owners
        .iter()
        .map(|owner| owner.upgrade().unwrap().rows.len())
        .sum::<usize>();
    let captured = captured_materialization_context(&context, vec![ids[first], ids[second]]);
    let expected = context
        .prepare_compilation(&scratch.path().join("expected"), &producer)
        .unwrap()
        .artifacts;
    if warm {
        let request = captured
            .prepare_compilation(&scratch.path().join("capture"), &producer)
            .unwrap();
        assert_eq!(
            request
                .materialization
                .as_ref()
                .unwrap()
                .payload_work
                .written_bytes,
            0
        );
        assert!(request
            .artifacts
            .iter()
            .all(|artifact| expected.contains(artifact)));
    }
    let fork = captured_materialization_context(&captured, vec![ids[second]]);
    drop(context);
    let selected_directories = BTreeSet::from([first + 1, second + 1]);
    let selected_census = directories
        .iter()
        .filter(|root| root.is_dir())
        .map(|root| private_directory_census(root))
        .fold((0, 0), |left, right| (left.0 + right.0, left.1 + right.1));
    eprintln!(
        "{}",
        serde_json::json!({
            "kind": "retained_materialization_capture_census", "history_length": length,
            "capture_roots": [first, second], "materialized_before_reselection": warm,
            "before": { "directories": directories.len(), "files": before.0, "bytes": before.1, "retained_rows": history_rows },
            "captured": { "directories": directories.iter().filter(|root| root.is_dir()).count(), "files": selected_census.0, "bytes": selected_census.1 },
            "required_directories": selected_directories.len(),
            "historical_materialization_owners_alive": owners.iter().filter(|owner| owner.upgrade().is_some()).count(),
            "row_struct_bytes_excludes_shared_payload_and_map_nodes": std::mem::size_of::<RetainedArtifactRow>(),
            "materialization_struct_bytes_excludes_collections_and_shared_payload": std::mem::size_of::<RetainedArtifactMaterialization>(),
            "graph_file_struct_bytes_excludes_path_buffer_and_directory": std::mem::size_of::<OwnedExecutionGraphFile>(),
        })
    );
    assert!(
        owners.iter().all(|owner| owner.upgrade().is_none()),
        "a capture must not retain historical materialization metadata"
    );
    for (index, root) in directories.iter().enumerate() {
        assert_eq!(
            root.is_dir(),
            selected_directories.contains(&index),
            "directory {index} retained beyond its selected facts"
        );
    }
    drop(captured);
    for (index, root) in directories.iter().enumerate() {
        assert_eq!(
            root.is_dir(),
            index == second + 1,
            "reselection retains only its required directory"
        );
    }
    let request = fork
        .prepare_compilation(&scratch.path().join("fork"), &producer)
        .unwrap();
    assert_eq!(request.artifacts.len(), 1);
    assert_eq!(
        request
            .materialization
            .as_ref()
            .unwrap()
            .payload_work
            .written_bytes,
        0
    );
    assert!(expected.contains(&request.artifacts[0]));
    assert!(request.artifacts[0].interface.path.is_file());
    drop(request);
    drop(fork);
    assert!(
        directories.iter().all(|root| !root.exists()),
        "last capture releases its issued directories"
    );
}

#[test]
fn tiny_materialized_capture_releases_unrelated_history_and_reselects_exact_files() {
    for length in [1, 8, 24] {
        check_materialized_capture_history(length, 0, length - 1, true);
    }
}

proptest::proptest! {
    #![proptest_config({ let mut config = policy_property_config(); config.cases = 48; config })]
    #[test]
    fn materialized_capture_select_fork_drop_matches_directory_oracle(
        length in 1usize..9, first in 0usize..8, second in 0usize..8, warm in proptest::bool::ANY,
    ) {
        check_materialized_capture_history(length, first % length, second % length, warm);
    }
}

#[test]
fn selected_rows_keep_same_issuance_directory_files_until_last_capture() {
    let (context, producer) = metadata_fixture();
    let scratch = tempfile::tempdir().unwrap();
    let request = context
        .prepare_compilation(&scratch.path().join("full"), &producer)
        .unwrap();
    let directory = request
        .materialization
        .as_ref()
        .unwrap()
        .directory()
        .path()
        .to_path_buf();
    let before = private_directory_census(&directory);
    let selected = context.compiler_metadata_snapshot().unwrap().entries
        [&identity("fixture", "Beta")]
        .descriptor
        .id;
    let capture = captured_materialization_context(&context, vec![selected]);
    let weak = Arc::downgrade(request.materialization.as_ref().unwrap());
    drop(request);
    drop(context);
    assert!(weak.upgrade().is_none());
    assert_eq!(
        private_directory_census(&directory),
        before,
        "same-directory extra files retain their issuance owner's lifetime"
    );
    let request = capture
        .prepare_compilation(&scratch.path().join("selected"), &producer)
        .unwrap();
    assert_eq!(request.artifacts.len(), 1);
    assert!(request.artifacts[0].interface.path.starts_with(&directory));
    assert_eq!(
        request
            .materialization
            .as_ref()
            .unwrap()
            .payload_work
            .written_bytes,
        0
    );
    drop(request);
    drop(capture);
    assert!(!directory.exists());
}

#[test]
fn captured_dependency_graph_files_keep_their_issued_directories() {
    let source = tempfile::tempdir().unwrap();
    let (graph_b, originals) = crate::execution_source::test_prepared_graph(source.path());
    let owners = originals
        .iter()
        .map(|original| original.owner.clone())
        .collect::<Vec<_>>();
    let graph_a = crate::execution_source::test_graph_requiring_original(
        &graph_b,
        &owners[1],
        graph_b.digest(),
    );
    let a = execution_entry(originals[0].clone(), Arc::clone(&graph_a));
    let b = execution_entry(originals[1].clone(), Arc::clone(&graph_b));
    let product = |entry: &ArtifactEntry| match &entry.payload {
        ArtifactPayload::Original(product) => product.clone(),
        _ => unreachable!(),
    };
    let mut context = Arc::new(
        ExactDeclarationContext::new(&[], &[], vec![])
            .unwrap()
            .extend_checked_original_products([7; 32], &[product(&b)])
            .unwrap(),
    );
    // This exercises the actual materialization issuer with already certified
    // fixture originals. It needs no compiler endpoint or new proof factory.
    let materialize = |context: &Arc<ExactDeclarationContext>| {
        let metadata = context.compiler_metadata_snapshot().unwrap();
        context
            .facts
            .compiler_support
            .artifacts()
            .retain_materialization(&metadata, |parents| {
                context.materialize_retained_artifacts(
                    &metadata,
                    parents,
                    tempfile::tempdir().unwrap(),
                )
            })
            .unwrap()
    };
    let base = materialize(&context);
    assert_eq!(base.graph_paths.len(), 1);
    let b_path = base.graph_paths[&graph_b.digest()].path.clone();
    let b_directory = base.directory().path().to_path_buf();
    let weak_b = Arc::downgrade(&base);
    drop(base);
    let unrelated = crate::certified_products::fixture_finalized_product(
        support_product("UnrelatedGraphHistory"),
        [7; 32],
    );
    context = Arc::new(
        context
            .as_ref()
            .clone()
            .extend_checked_original_products([7; 32], &[unrelated])
            .unwrap(),
    );
    let unrelated = materialize(&context);
    let unrelated_directory = unrelated.directory().path().to_path_buf();
    let weak_unrelated = Arc::downgrade(&unrelated);
    drop(unrelated);
    context = Arc::new(
        context
            .as_ref()
            .clone()
            .extend_checked_original_products([7; 32], &[product(&a)])
            .unwrap(),
    );
    let current = materialize(&context);
    let a_path = current.graph_paths[&graph_a.digest()].path.clone();
    let a_directory = current.directory().path().to_path_buf();
    let weak_a = Arc::downgrade(&current);
    assert_eq!(current.graph_path_refs().len(), 2);
    let captured =
        captured_materialization_context(&context, vec![a.descriptor.id, b.descriptor.id]);
    drop(current);
    drop(context);
    assert!(
        weak_a.upgrade().is_none()
            && weak_b.upgrade().is_none()
            && weak_unrelated.upgrade().is_none()
    );
    assert!(!unrelated_directory.exists());
    let owner = materialize(&captured);
    assert_eq!(owner.payload_work.written_bytes, 0);
    assert!(
        owner.graph_paths.is_empty(),
        "the capture rewrites no dependency graphs"
    );
    let paths = owner.graph_path_refs();
    assert_eq!(paths.len(), 2);
    assert_eq!(paths[&graph_a.digest()].path, a_path);
    assert_eq!(paths[&graph_b.digest()].path, b_path);
    let scope = owner.execution_scope.as_ref().unwrap().as_array().unwrap();
    assert_eq!(scope[0].as_array().unwrap().len(), 2);
    assert_eq!(
        scope[1].as_array().unwrap().len(),
        2,
        "the dependent and required original stay executable"
    );
    assert_eq!(std::fs::read(&a_path).unwrap(), graph_a.bytes());
    assert_eq!(std::fs::read(&b_path).unwrap(), graph_b.bytes());
    drop(captured);
    assert!(a_directory.is_dir() && b_directory.is_dir());
    drop(owner);
    assert!(!a_directory.exists() && !b_directory.exists());
}

#[test]
fn late_private_native_literals_and_graphs_survive_detached_capture() {
    use crate::certified_products::PendingImportOwner;
    use tidepool_repr::execution_schema::{Group, HeapRhs};
    let source = tempfile::tempdir().unwrap();
    let (graph_b, originals) =
        crate::execution_source::test_prepared_graph_with_interfaces(source.path(), |module| {
            format!("{module} graph interface").into_bytes()
        });
    let graph_a = crate::execution_source::test_graph_requiring_original(
        &graph_b,
        &originals[1].owner,
        graph_b.digest(),
    );
    let a = execution_entry(originals[0].clone(), Arc::clone(&graph_a));
    let b = execution_entry(originals[1].clone(), Arc::clone(&graph_b));
    let helper = crate::certified_products::tests::original_groups_fixture_with_interface(
        "LateNativeHelper",
        vec![(7, vec![])],
        11,
        &BTreeMap::new(),
        b"late helper interface".to_vec(),
    );
    let import = PendingImportOwner::Source {
        owner: helper.owner().clone(),
        original_ordinal: 7,
        binder: tidepool_repr::execution_schema::testing::identity("LateNativeHelper", "entry"),
    };
    let literal = [0, 17, 128, 255, 42];
    let root = crate::certified_products::tests::original_groups_fixture_in_unit_with_sites(
        "fixture",
        "LateNativeRoot",
        vec![(7, vec![import.clone()])],
        12,
        &BTreeMap::new(),
        b"late root interface".to_vec(),
        &BTreeMap::new(),
        &literal,
    );
    let products = crate::certified_products::tests::recovered_witness_fixtures(&[
        crate::certified_products::fixture_finalized_product(helper, [7; 32]),
        crate::certified_products::fixture_finalized_product(root, [7; 32]),
    ]);
    let root_product_bytes = products[1].product.product_bytes().to_vec();
    let mut entries = products
        .into_iter()
        .map(|original| Arc::new(ArtifactEntry::original([7; 32], original.product).unwrap()))
        .collect::<Vec<_>>();
    let root_id = entries[1].descriptor.id;
    entries.extend([Arc::clone(&a), Arc::clone(&b)]);
    let roots = vec![root_id, a.descriptor.id, b.descriptor.id];
    let graph_bytes = BTreeMap::from([
        (graph_a.digest(), graph_a.bytes().to_vec()),
        (graph_b.digest(), graph_b.bytes().to_vec()),
    ]);
    // These existing graph fixtures carry a test producer digest. Exercise
    // the production host input/materialization owners without pretending to
    // qualify a compiler endpoint whose producer bytes cannot match it.
    let ancestor = Arc::new(
        ExactDeclarationContext::new(&[], &[], vec![])
            .unwrap()
            .extend_checked_original_products(
                [7; 32],
                &[crate::certified_products::fixture_finalized_product(
                    support_product("PrivateBase"),
                    [7; 32],
                )],
            )
            .unwrap(),
    );
    let extend = |context: &Arc<ExactDeclarationContext>, module: &str| {
        Arc::new(
            context
                .as_ref()
                .clone()
                .extend_checked_original_products(
                    [7; 32],
                    &[crate::certified_products::fixture_finalized_product(
                        support_product(module),
                        [7; 32],
                    )],
                )
                .unwrap(),
        )
    };
    let materialize = |context: &Arc<ExactDeclarationContext>,
                       metadata: &CompilerArtifactMetadata| {
        context
            .facts
            .compiler_support
            .artifacts()
            .retain_materialization(metadata, |parents| {
                context.materialize_retained_artifacts(
                    metadata,
                    parents,
                    tempfile::tempdir().unwrap(),
                )
            })
            .unwrap()
    };
    let private_inputs = |context: &Arc<ExactDeclarationContext>| {
        // AllGroups is the existing issuer of exact native roots, not an
        // inference from available code or a test certificate constructor.
        let artifacts = context
            .facts
            .compiler_support
            .artifacts()
            .inventory()
            .admit_shared(context.facts.compiler_support.artifacts(), entries.clone())
            .unwrap();
        let projection = CompilerInputProjection::from_issued_entries(&entries).unwrap();
        let requirements = crate::artifact_inventory::NativeOriginalRequirement::for_projection(
            &projection,
            &artifacts,
            &[],
        )
        .unwrap();
        let native_offers = crate::artifact_inventory::IssuedNativeOffer::issue_context_offers(
            &projection,
            &artifacts,
            &requirements,
        )
        .unwrap();
        let private = OriginalCompilerInputs {
            projection,
            artifacts,
            native_offers,
        };
        let support =
            crate::artifact_inventory::CompilerSourceSupport::close_complete_output(&private)
                .unwrap();
        let inputs = ProtectedScaffoldRequirements::default()
            .compiler_inputs(context, Some(&private))
            .unwrap();
        (inputs, support)
    };
    let descendant = extend(&ancestor, "BeforePrivate");
    let completed = materialize(
        &descendant,
        &descendant.compiler_metadata_snapshot().unwrap(),
    );
    assert!(completed.groups.is_empty());
    assert!(completed.graph_paths.is_empty());
    let completed_directory = completed.directory().path().to_path_buf();
    let (late_inputs, late_support) = private_inputs(&ancestor);
    assert_eq!(
        late_inputs.metadata.retained.selected_native_groups.len(),
        2
    );
    let late = materialize(&ancestor, &late_inputs.metadata);
    let late_directory = late.directory().path().to_path_buf();
    let next = extend(&descendant, "AfterPrivate");
    let (inputs, support) = private_inputs(&next);
    assert_ne!(
        inputs.metadata.materialization_key(),
        descendant
            .compiler_metadata_snapshot()
            .unwrap()
            .materialization_key()
    );
    let current = materialize(&next, &inputs.metadata);
    let current_directory = current.directory().path().to_path_buf();
    let check_native = |owner: &RetainedArtifactMaterialization,
                        metadata: &CompilerArtifactMetadata| {
        assert_eq!(metadata.retained.selected_native_groups.len(), 2);
        let selected =
            RetainedArtifactMaterialization::selected_group_refs([owner], &metadata.retained);
        assert_eq!(selected.len(), 2);
        let root = selected
            .iter()
            .find(|group| group.owner().module == "LateNativeRoot")
            .unwrap();
        assert_eq!(root.imports(), std::slice::from_ref(&import));
        let bindings = root.group().definitions().bindings();
        let Group::Recursive(tops) = &bindings[0] else {
            panic!("fixture literal group")
        };
        assert_eq!(tops[0].binding.rhs, HeapRhs::Bytes(literal.to_vec()));
        let artifact = owner.selected_rows(metadata)[&root_id].artifact();
        assert_eq!(
            std::fs::read(artifact.product.unwrap().path).unwrap(),
            root_product_bytes
        );
    };
    check_native(&late, &late_inputs.metadata);
    check_native(&current, &inputs.metadata);
    let paths = current
        .graph_path_refs()
        .into_iter()
        .map(|(digest, file)| (digest, file.path.clone()))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(paths.len(), 2);
    for (digest, bytes) in &graph_bytes {
        assert_eq!(std::fs::read(&paths[digest]).unwrap(), *bytes);
    }
    let mut captured = next.as_ref().clone();
    let selected = support
        .retain_custody(&inputs.artifacts)
        .unwrap()
        .select_support_roots(roots)
        .unwrap();
    captured = captured.rebuild(|facts| {
        facts.compiler_support = selected;
        facts.lexical = crate::exact_context::ExactLexicalSnapshot::default();
    });
    captured.normalize().unwrap();
    let captured = Arc::new(captured);
    let payload = Arc::downgrade(
        &captured
            .facts
            .compiler_support
            .artifacts()
            .metadata_snapshot()
            .artifacts[&root_id],
    );
    let proof = Arc::clone(
        captured
            .facts
            .compiler_support
            .artifacts()
            .selection_snapshot(),
    );
    let weak_current = Arc::downgrade(&current);
    let weak_late = Arc::downgrade(&late);
    drop(inputs);
    drop(support);
    drop(late_inputs);
    drop(late_support);
    drop(current);
    drop(late);
    drop(completed);
    drop(next);
    drop(descendant);
    drop(ancestor);
    drop(entries);
    drop(a);
    drop(b);
    drop(graph_a);
    drop(graph_b);
    drop(originals);
    std::fs::remove_dir_all(source.path()).unwrap();
    assert!(weak_current.upgrade().is_none() && weak_late.upgrade().is_none());
    assert!(!completed_directory.exists() && !late_directory.exists());
    assert!(current_directory.is_dir());
    for path in paths.values() {
        assert!(path.is_file());
    }
    let restored_metadata = captured.compiler_metadata_snapshot().unwrap();
    let restored = materialize(&captured, &restored_metadata);
    check_native(&restored, &restored_metadata);
    assert_eq!(restored.payload_work.written_bytes, 0);
    let restored_paths = restored.graph_path_refs();
    assert_eq!(restored_paths.len(), 2);
    for (digest, bytes) in &graph_bytes {
        assert_eq!(restored_paths[digest].path, paths[digest]);
        assert_eq!(std::fs::read(&paths[digest]).unwrap(), *bytes);
    }
    drop(restored_metadata);
    drop(captured);
    assert!(current_directory.is_dir());
    drop(restored);
    assert!(!current_directory.exists());
    assert!(paths.values().all(|path| !path.exists()));
    assert!(payload.upgrade().is_none());
    assert!(!proof.scopes.is_empty());
}
