//! Publication recovery and narrowing preserve exact selected native and type custody.
use super::*;
use crate::artifact_inventory::NativeGroupKey;
use crate::certified_products::PendingImportOwner;
use proptest::prelude::*;

const ORDINALS: [u32; 3] = [3, 11, 29];

fn fixture(
    initial: usize,
) -> (
    Arc<ArtifactEntry>,
    Arc<ArtifactEntry>,
    Arc<PublishedSourceOriginalSelection>,
) {
    fixture_with_helper_version(initial, 1)
}

fn fixture_with_helper_version(
    initial: usize,
    helper_version: u8,
) -> (
    Arc<ArtifactEntry>,
    Arc<ArtifactEntry>,
    Arc<PublishedSourceOriginalSelection>,
) {
    let (root, helper, publication, _) = fixture_with_source(initial, helper_version);
    (root, helper, publication)
}

fn fixture_with_source(
    initial: usize,
    helper_version: u8,
) -> (
    Arc<ArtifactEntry>,
    Arc<ArtifactEntry>,
    Arc<PublishedSourceOriginalSelection>,
    ExactDeclarationContext,
) {
    let producer = [2; 32];
    let helper = Arc::new(
        ArtifactEntry::original(
            producer,
            crate::certified_products::tests::recovered_witness_fixtures(&[
                crate::certified_products::fixture_finalized_product(
                    crate::certified_products::tests::original_groups_fixture_with_interface(
                        "HistoryHelper",
                        vec![(7, vec![])],
                        helper_version,
                        &BTreeMap::new(),
                        b"HistoryHelper interface".to_vec(),
                    ),
                    producer,
                ),
            ])
            .remove(0)
            .product,
        )
        .unwrap(),
    );
    let ArtifactPayload::Original(helper_product) = &helper.payload else {
        unreachable!()
    };
    let helper_definitions =
        crate::certified_products::original_native_group_definitions(helper_product, 7).unwrap();
    let helper_binding = match &helper_definitions.bindings()[0] {
        tidepool_repr::execution_schema::Group::NonRecursive(binding) => binding,
        tidepool_repr::execution_schema::Group::Recursive(bindings) => &bindings[0],
    };
    let helper_import = PendingImportOwner::Source {
        owner: helper_product.owner().clone(),
        original_ordinal: 7,
        binder: helper_binding.identity.clone(),
    };
    let product = crate::certified_products::tests::recovered_witness_fixtures(&[
        crate::certified_products::fixture_finalized_product_with_requirements(
            crate::certified_products::tests::original_groups_fixture(
                "HistoryRoot",
                ORDINALS
                    .iter()
                    .map(|ordinal| {
                        (
                            *ordinal,
                            if *ordinal == 3 {
                                vec![]
                            } else {
                                vec![helper_import.clone()]
                            },
                        )
                    })
                    .collect(),
                1,
                &BTreeMap::new(),
            ),
            producer,
            Some(BTreeMap::from([(
                ("fixture".into(), "HistoryHelper".into()),
                helper.descriptor.interface_sha256,
            )])),
        ),
    ])
    .remove(0)
    .product;
    let root = Arc::new(ArtifactEntry::original(producer, product).unwrap());
    let unrelated = Arc::new(
        ArtifactEntry::original(
            producer,
            crate::certified_products::tests::recovered_witness_fixtures(&[
                crate::certified_products::fixture_finalized_product(
                    crate::certified_products::tests::original_groups_fixture(
                        "HistoryUnrelated",
                        vec![(7, vec![])],
                        1,
                        &BTreeMap::new(),
                    ),
                    producer,
                ),
            ])
            .remove(0)
            .product,
        )
        .unwrap(),
    );
    let target = Arc::new(ArtifactEntry::canonical(
        crate::certified_products::fixture_module_interface(
            producer,
            "fixture",
            "HistoryTarget",
            BTreeMap::new(),
        ),
    ));
    let inventory = ArtifactInventory::default();
    let view = inventory
        .admit_recovery_selection(
            &inventory.empty_view(),
            vec![
                root.clone(),
                helper.clone(),
                target.clone(),
                unrelated.clone(),
            ],
            &std::iter::once(NativeGroupKey {
                artifact: root.descriptor.id,
                original_ordinal: ORDINALS[initial],
            })
            .chain((initial != 0).then_some(NativeGroupKey {
                artifact: helper.descriptor.id,
                original_ordinal: 7,
            }))
            .chain(std::iter::once(NativeGroupKey {
                artifact: unrelated.descriptor.id,
                original_ordinal: 7,
            }))
            .collect(),
        )
        .unwrap();
    let issued = ExactDeclarationContext::from_authenticated_execution(
        producer,
        &view,
        CompilerInputProjection::from_issued_entries(&[
            root.clone(),
            helper.clone(),
            target.clone(),
            unrelated.clone(),
        ])
        .unwrap(),
        vec![
            ExactLexicalNode {
                owner: target.descriptor.owner.clone(),
                imports: vec![root.descriptor.owner.clone()],
            },
            ExactLexicalNode {
                owner: root.descriptor.owner.clone(),
                imports: vec![helper.descriptor.owner.clone()],
            },
            ExactLexicalNode {
                owner: helper.descriptor.owner.clone(),
                imports: vec![],
            },
            ExactLexicalNode {
                owner: unrelated.descriptor.owner.clone(),
                imports: vec![],
            },
        ],
        target.descriptor.owner.clone(),
        &[
            target.descriptor.owner.clone(),
            root.descriptor.owner.clone(),
            helper.descriptor.owner.clone(),
            unrelated.descriptor.owner.clone(),
        ],
    )
    .unwrap();
    let publication = issued
        .issue_published_source_original(
            "history-revision",
            "history-input",
            &root.descriptor.owner,
        )
        .unwrap();
    assert!(
        !publication
            .context()
            .artifact_view()
            .artifact_ids()
            .contains(&unrelated.descriptor.id),
        "first birth excludes unrelated native custody and its ambient offer"
    );
    assert!(!publication
        .context()
        .compiler_input_roles()
        .iter()
        .any(|role| role.original() == Some(unrelated.descriptor.id)));
    (root, helper, publication, issued)
}

#[test]
fn publication_reissue_keeps_birth_when_current_roles_change() {
    let (root, helper, publication, source) = fixture_with_source(1, 1);
    let source = source
        .with_published_source_originals(&publication)
        .unwrap();
    let owners = BTreeSet::from([
        root.descriptor.owner.clone(),
        source.original_instance_target().unwrap().clone(),
    ]);
    let support = source
        .compiler_source_support()
        .source_support(&owners)
        .unwrap();
    let lexical = source
        .lexical_graph()
        .iter()
        .filter(|node| node.owner != identity("fixture", "HistoryUnrelated"))
        .cloned()
        .collect();
    let source = source
        .rebuild(|facts| {
            facts.compiler_support = support;
            facts.lexical = facts.lexical.replace(lexical).unwrap();
        })
        .finish()
        .unwrap();
    assert!(source
        .compiler_input_roles()
        .iter()
        .any(|role| role.interface()
            == publication
                .context()
                .compiler_input_projection()
                .snapshot_roles()[&helper.descriptor.owner]
                .interface()
            && role.original().is_none()));
    let before = &publication
        .context()
        .compiler_input_projection()
        .snapshot_roles()[&root.descriptor.owner];
    let reissued = source
        .issue_published_source_original(
            "history-revision",
            "history-input",
            &root.descriptor.owner,
        )
        .unwrap();
    assert_eq!(
        &reissued
            .context()
            .compiler_input_projection()
            .snapshot_roles()[&root.descriptor.owner],
        before
    );
    assert!(reissued
        .context()
        .compiler_input_roles()
        .iter()
        .all(|role| role.original() != Some(helper.descriptor.id)));
    assert!(source
        .issue_published_source_original(
            "conflicting-revision",
            "history-input",
            &root.descriptor.owner,
        )
        .is_err());
}

pub(super) fn restored_graph_roles(
    context: &ExactDeclarationContext,
    roles: &[CompilerInputRole],
) -> Result<
    (
        tempfile::TempDir,
        RecoveredArtifactInventory,
        ExactDeclarationContext,
    ),
    CompileError,
> {
    let view = context.artifact_view();
    let scratch = tempfile::tempdir().unwrap();
    let (products, interfaces) = partition_native_recovery_references(
        context
            .materialize_recovery_artifacts(
                scratch.path(),
                &context.artifact_view().artifact_ids().into_iter().collect(),
            )
            .unwrap(),
    );
    let selection: crate::artifact_inventory::ArtifactGraphSelection =
        serde_json::from_slice(&serde_json::to_vec(&view.capture_graph_selection()).unwrap())
            .unwrap();
    let edges: Vec<_> =
        serde_json::from_slice(&serde_json::to_vec(&view.binding_dependencies()).unwrap()).unwrap();
    let inventory = RecoveredArtifactInventory::capture_bound(
        scratch.path(),
        &products,
        &interfaces,
        &[],
        &[],
        &view.descriptors(),
        &selection.bindings,
        &edges,
    )
    .expect("history recovery: certificate acquisition");
    let authenticated = inventory
        .context_with_graph_selection(&selection, roles, context.owner.facts.lexical.to_vec())
        .map_err(|error| {
            eprintln!("history recovery stage=graph-role-authentication error={error:?}");
            error
        })?;
    Ok((scratch, inventory, authenticated))
}

fn property_config() -> proptest::test_runner::Config {
    let mut config = proptest::test_runner::Config::default();
    if let Some(path) = option_env!("TIDEPOOL_PROPTEST_REGRESSIONS") {
        config.failure_persistence = Some(Box::new(
            proptest::test_runner::FileFailurePersistence::Direct(path),
        ));
    }
    config
}

#[test]
fn publication_reopening_refuses_incomplete_lexical_type_namespace() {
    let (root, helper, publication) = fixture(0);
    let context = publication.context();
    assert!(!context
        .published_source_original_selections()
        .unwrap()
        .is_empty());
    let mut lexical = context.lexical_graph().to_vec();
    lexical
        .iter_mut()
        .find(|node| node.owner == root.descriptor.owner)
        .unwrap()
        .imports
        .clear();
    let mut changed = context.as_ref().clone().rebuild(|facts| {
        facts.lexical = facts.lexical.replace(lexical).unwrap();
    });
    let mut projection = CompilerInputProjection::default();
    for mut role in changed.compiler_input_roles() {
        if role.is_published_source_original() {
            let facts = PublishedSelectionFacts::from_role(&role).unwrap();
            let digest = changed
                .published_selection_sha256(&root.descriptor.owner, &facts)
                .unwrap();
            if let CompilerInputRole::PublishedSourceOriginal {
                selection_sha256, ..
            } = &mut role
            {
                *selection_sha256 = digest;
            }
        }
        let owner = changed.artifact_view().metadata_snapshot().artifacts[&role.interface()]
            .descriptor
            .owner
            .clone();
        projection.admit_role(owner, role).unwrap();
    }
    changed = changed.recover_fixture_projection(projection).unwrap();
    let reopened = changed.published_source_original_selections().unwrap();
    assert!(
        reopened[0]
            .context()
            .compiler_input_roles()
            .iter()
            .any(|role| role.interface()
                == context.compiler_input_projection().snapshot_roles()[&helper.descriptor.owner]
                    .interface()
                && role.original().is_none()),
        "type dependencies survive independently of lexical imports"
    );
    let assert_missing_type_role = |result: Result<ExactDeclarationContext, CompileError>| {
        let Err(CompileError::ArtifactInventory(error)) = result else {
            panic!("incomplete publication must refuse its required type role: {result:?}")
        };
        assert_eq!(
            error.failure,
            crate::artifact_inventory::ArtifactInventoryFailure::MissingDependency {
                artifact: root.descriptor.id,
                dependent: root.descriptor.owner.clone(),
                required: helper.descriptor.owner.clone(),
                dependency: crate::artifact_inventory::ArtifactDependency::Interface,
            }
        );
    };
    let mut incomplete = CompilerInputProjection::default();
    incomplete
        .admit_role(
            root.descriptor.owner.clone(),
            changed.compiler_input_projection().snapshot_roles()[&root.descriptor.owner].clone(),
        )
        .unwrap();
    let canonical =
        context.compiler_input_projection().snapshot_roles()[&helper.descriptor.owner].interface();
    let mut graph = changed.artifact_view().capture_graph_selection();
    graph.namespace.retain(|binding| {
        binding.artifact != canonical && binding.artifact != helper.descriptor.id
    });
    let view = changed
        .artifact_view()
        .select_issued(
            vec![root.descriptor.id],
            &crate::artifact_inventory::ExactArtifactSelection::from_recovered_graph_selection(
                graph.clone(),
            ),
        )
        .unwrap();
    assert_missing_type_role(
        recover_fixture_compiler_support(
            incomplete,
            view,
            changed
                .compiler_source_support()
                .native_offers()
                .iter()
                .cloned(),
        )
        .and_then(|support| {
            changed
                .clone()
                .rebuild(|facts| facts.compiler_support = support)
                .finish()
        }),
    );
    let (_scratch, inventory, _) =
        restored_graph_roles(&changed, &changed.compiler_input_roles()).unwrap();
    let mut snapshot = changed.snapshot();
    snapshot.native_fact_owner = None;
    snapshot.roles.remove(&helper.descriptor.owner);
    snapshot.selection =
        Arc::new(crate::exact_context::ExactSelectionSnapshot::from_graph_selection(&graph));
    let store = fvm_ipld_blockstore::MemoryBlockstore::default();
    let mut writer = crate::exact_context::Materializer::new(&store);
    let written = writer.materialize_context(None, &snapshot).unwrap();
    use fvm_ipld_blockstore::Blockstore;
    for (cid, bytes) in writer.finish().unwrap().into_blocks() {
        store.put_keyed(&cid, &bytes).unwrap();
    }
    let decoded = crate::exact_context::hydrate_context(&store, written.root()).unwrap();
    assert_missing_type_role(inventory.context_for_expected_snapshot(written.root(), &decoded));
}

#[test]
fn published_role_refuses_missing_issuer_selection() {
    let (_, _, publication) = fixture(0);
    let mut roles = serde_json::to_value(publication.context().compiler_input_roles()).unwrap();
    let published = publication
        .context()
        .compiler_input_roles()
        .into_iter()
        .position(|role| role.is_published_source_original())
        .expect("fixture publication carries an exact source selection");
    roles[published]
        .as_object_mut()
        .unwrap()
        .remove("selection");
    assert!(
        serde_json::from_value::<Vec<CompilerInputRole>>(roles).is_err(),
        "legacy publication facts cannot reconstruct the issuer's selection from broader custody"
    );
}

proptest! {
    #![proptest_config(property_config())]
    #[test]
    fn published_source_narrowing_preserves_native_dependency_custody(
        initial in 1usize..ORDINALS.len(), rounds in 1usize..5,
    ) {
        use fvm_ipld_blockstore::Blockstore;
        let (root, helper, publication) = fixture(initial);
        let original = publication.context();
        let expected_graph = original.artifact_view().capture_graph_selection();
        let expected_edges = original.artifact_view().binding_dependencies();
        let expected_role = original.compiler_input_roles().into_iter()
            .find(|role| role.is_published_source_original()).unwrap();
        let helper_role = original.compiler_input_roles().into_iter()
            .find(|role| role.original() == Some(helper.descriptor.id)).unwrap();
        let canonical = helper_role.interface();
        let canonical_binding = *expected_graph.namespace.iter()
            .find(|binding| binding.artifact == canonical).unwrap();
        prop_assert!(expected_graph.native_groups.iter()
            .any(|group| group.binding.artifact() == helper.descriptor.id));
        let owners = BTreeSet::from([root.descriptor.owner.clone()]);
        let mut context = original.as_ref().clone();
        for _ in 0..rounds {
            // Narrow through the owning source operation, preserving its rows,
            // offers and exact graph rather than reconstructing fixture roles.
            let support = context.compiler_source_support().source_support(&owners).unwrap();
            let role = support.projection().roles().into_iter()
                .find(|role| role.interface() == canonical).unwrap();
            prop_assert_eq!(role.original(), None);
            let graph = support.artifacts().capture_graph_selection();
            prop_assert!(graph.namespace.contains(&canonical_binding));
            for binding in &expected_graph.bindings {
                prop_assert!(graph.bindings.contains(binding));
            }
            for group in &expected_graph.native_groups {
                prop_assert!(graph.native_groups.contains(group));
            }
            let edges = support.artifacts().binding_dependencies();
            for edge in &expected_edges {
                prop_assert!(edges.contains(edge));
            }
            context = context.rebuild(|facts| facts.compiler_support = support).finish().unwrap();
            let roles = context.compiler_input_roles();
            prop_assert!(roles.contains(&expected_role));
            prop_assert!(roles.iter().any(|role| role.interface() == canonical && role.original().is_none()));
            let reopened = context.published_source_original_selections().unwrap();
            prop_assert_eq!(reopened.len(), 1);
            prop_assert!(reopened[0].context().compiler_input_roles().contains(&expected_role));
            prop_assert!(reopened[0].context().compiler_input_roles().iter()
                .any(|role| role.interface() == canonical && role.original().is_none()),
                "reopening must not restore the birth's withdrawn Original role");
            prop_assert_eq!(reopened[0].context().artifact_view().selected_native_groups(),
                context.artifact_view().selected_native_groups());
            prop_assert_eq!(reopened[0].context().compiler_source_support()
                .publication_birth_support(&root.descriptor.owner).unwrap()
                .artifacts().capture_graph_selection(), expected_graph.clone());

            let store = fvm_ipld_blockstore::MemoryBlockstore::default();
            let mut writer = crate::exact_context::Materializer::new(&store);
            let persisted = writer.materialize_context(None, &context.snapshot()).unwrap();
            for (cid, bytes) in writer.finish().unwrap().into_blocks() {
                store.put_keyed(&cid, &bytes).unwrap();
            }
            let view = context.artifact_view();
            let graph = view.capture_graph_selection();
            let scratch = tempfile::tempdir().unwrap();
            let (products, interfaces) = partition_native_recovery_references(
                context.materialize_recovery_artifacts(
                    scratch.path(),
                    &context.artifact_view().artifact_ids().into_iter().collect(),
                ).unwrap(),
            );
            let inventory = RecoveredArtifactInventory::capture_bound(
                scratch.path(), &products, &interfaces, &[], &[],
                &view.descriptors(), &graph.bindings, &view.binding_dependencies(),
            ).unwrap();
            let acquired = crate::exact_context::hydrate_context(&store, persisted.root()).unwrap();
            context = inventory.context_for_expected_snapshot(persisted.root(), &acquired).unwrap();
            prop_assert_eq!(context.compiler_input_roles(), roles);
            let reopened = context.published_source_original_selections().unwrap();
            prop_assert!(reopened[0].context().compiler_input_roles().contains(&expected_role));
            prop_assert!(reopened[0].context().compiler_input_roles().iter()
                .any(|role| role.interface() == canonical && role.original().is_none()),
                "cold recovery must not restore the birth's withdrawn Original role");
        }
        // A separately authenticated native version with the same canonical
        // interface is still a real selected-original conflict, not type-only custody.
        let (_, other_helper, other_publication) = fixture_with_helper_version(initial, 2);
        prop_assert_eq!(other_helper.descriptor.interface_sha256, helper.descriptor.interface_sha256);
        prop_assert_ne!(other_helper.descriptor.id, helper.descriptor.id);
        let other = other_publication.context().compiler_source_support()
            .source_support(&BTreeSet::from([helper.descriptor.owner.clone()])).unwrap();
        prop_assert!(other.projection().roles().iter()
            .any(|role| role.interface() == canonical
                && role.original() == Some(other_helper.descriptor.id)));
        let before = context.semantic_sha256();
        let Err(CompileError::ExtractFailed(detail)) =
            context.clone().extend_issued_compiler_support(&other)
        else { panic!("a competing selected original must refuse publication reopening") };
        prop_assert!(detail.starts_with("exact declaration context: published selection changes a selected original dependency"));
        prop_assert!(detail.contains(&format!("{:?}", helper.descriptor.owner)), "refusal identifies the dependency owner");
        prop_assert!(detail.contains(&format!("{:?}", helper.descriptor.id)), "refusal identifies the issued original");
        prop_assert!(detail.contains(&format!("{:?}", other_helper.descriptor.id)), "refusal identifies the competing original");
        prop_assert_eq!(context.semantic_sha256(), before);
    }
}
