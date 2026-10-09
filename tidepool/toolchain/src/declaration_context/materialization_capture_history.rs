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
    let mut selected = source.as_ref().clone();
    selected.inventory = source.inventory.select_roots(roots).unwrap();
    selected.compiler_projection = source.compiler_projection.within_view(&selected.inventory);
    selected.lexical.clear();
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
            context.producer,
        );
        context = Arc::new(
            context
                .as_ref()
                .clone()
                .extend_checked_original_products(context.producer, &[product])
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
    let (graph_b, owners) = crate::execution_source::test_graph(source.path());
    let graph_a = crate::execution_source::test_graph_requiring_original(
        &graph_b,
        &owners[1],
        graph_b.digest(),
    );
    let a = execution_entry(owners[0].clone(), Arc::clone(&graph_a));
    let b = execution_entry(owners[1].clone(), Arc::clone(&graph_b));
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
            .inventory
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
