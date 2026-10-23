use super::*;
use crate::artifact_inventory::{NativeArtifactDemand, NativeGroupKey};
use crate::certified_products::PendingImportOwner;
use tidepool_repr::execution_schema::SymbolIdentity;

#[test]
fn published_source_composition_preserves_selected_issuer_over_historical_custody() {
    published_composition_histories(false, false);
}

#[test]
fn sparse_partial_publication_cannot_promote_unissued_later_group() {
    published_composition_histories(true, false);
}

#[test]
fn interface_adapter_preserves_issued_canonical_scope_and_refuses_unselected_attachment() {
    published_composition_histories(false, true);
}

fn check_interface_adapter(
    issued: &ExactDeclarationContext,
    selected_issuer: &ArtifactView,
    original: ArtifactId,
    child: ArtifactId,
) {
    use crate::artifact_inventory::{
        ArtifactBindingNode, ArtifactDependency, ExactArtifactSelection,
    };
    let root_owner = identity("fixture", "Root");
    let graph = issued.artifact_view().capture_graph_selection();
    let original_binding = *graph
        .namespace
        .iter()
        .find(|binding| binding.artifact == original)
        .unwrap();
    let canonical = issued.compiler_input_projection().snapshot_roles()[&root_owner].interface();
    let canonical_binding = issued
        .artifact_view()
        .binding_dependencies()
        .into_iter()
        .find_map(|(from, to, dependency)| match (from, to, dependency) {
            (
                ArtifactBindingNode::Artifact(from),
                ArtifactBindingNode::Artifact(to),
                ArtifactDependency::Interface,
            ) if from == original_binding && to.artifact == canonical => Some(to),
            _ => None,
        })
        .expect("the selected original owns its exact canonical attachment");
    let types = issued
        .compiler_source_support()
        .selected_interfaces()
        .unwrap()
        .support_for_owners(vec![root_owner.clone()])
        .unwrap();
    let interfaces = types.artifacts();
    assert!(interfaces
        .capture_graph_selection()
        .namespace
        .contains(&canonical_binding));
    assert!(interfaces.selected_native_groups().is_empty());
    let expected_edges = interfaces.binding_dependencies();
    let empty = ExactDeclarationContext::new(&[], &[], vec![]).unwrap();
    let extended = empty
        .clone()
        .extend_issued_compiler_support(&types)
        .expect("the checked-cell interface projection preserves its issuing scope");
    let roles = extended.compiler_input_projection().snapshot_roles();
    assert_eq!(roles[&root_owner].interface(), canonical);
    assert_eq!(roles[&identity("fixture", "Child")].interface(), child);
    assert_eq!(roles.len(), 2);
    assert!(roles.values().all(|role| role.original().is_none()));
    assert!(extended
        .artifact_view()
        .capture_graph_selection()
        .namespace
        .contains(&canonical_binding));
    assert_eq!(
        extended.artifact_view().binding_dependencies(),
        expected_edges
    );
    assert!(extended.artifact_view().selected_native_groups().is_empty());
    assert!(extended.lexical_graph().is_empty());

    // Recovery may transport a narrower namespace without changing any
    // authenticated binding, edge, root or custody fact. An attachment in
    // custody does not make it a selected interface input.
    assert_eq!(
        selected_issuer
            .capture_graph_selection()
            .bindings
            .iter()
            .filter(|binding| binding.artifact == canonical)
            .count(),
        1,
        "the clean issuer negative cannot refuse from canonical binding ambiguity"
    );
    for source in [selected_issuer, issued.artifact_view()] {
        let graph = source.capture_graph_selection();
        let mut original_only = graph.clone();
        original_only
            .namespace
            .retain(|binding| *binding == original_binding);
        let original_only = source
            .select_issued(
                vec![original],
                &ExactArtifactSelection::from_recovered_graph_selection(original_only),
            )
            .expect("the owner authenticates the unchanged issued graph");
        let narrowed = original_only.capture_graph_selection();
        assert_eq!(narrowed.namespace, vec![original_binding]);
        assert_eq!(narrowed.bindings, graph.bindings);
        assert_eq!(narrowed.roots, graph.roots);
        assert_eq!(narrowed.custody_roots, graph.custody_roots);
        assert_eq!(narrowed.native_groups, graph.native_groups);
        assert_eq!(
            original_only.binding_dependencies(),
            source.binding_dependencies()
        );
        assert!(narrowed.bindings.contains(&canonical_binding));
        assert!(
            matches!(crate::declaration_context::recover_fixture_compiler_support(
                types.projection().clone(), original_only, [],
            ), Err(CompileError::ExtractFailed(detail))
                if detail == "exact declaration context: bound recovery namespace differs from recorded compiler roles"),
            "recorded type roles must refuse a namespace that omits their selected canonical attachment"
        );
    }
}

fn assert_sparse_partial_transport(context: &ExactDeclarationContext, original: ArtifactId) {
    let offers = context.compiler_source_support().native_offers();
    let partials = offers
        .iter()
        .filter(|offer| offer.original() == original)
        .filter_map(|offer| match offer.snapshot_facts().body {
            crate::exact_context::NativeOfferSnapshotBody::Partial(selection) => Some(selection),
            crate::exact_context::NativeOfferSnapshotBody::Full(_) => {
                panic!("sparse issuance became Full")
            }
            crate::exact_context::NativeOfferSnapshotBody::Empty(_) => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(partials.len(), 1);
    assert_eq!(
        partials[0]
            .graph_selection()
            .native_groups
            .iter()
            .filter(|group| group.binding.artifact() == original)
            .map(|group| group.original_ordinal)
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([7])
    );
    assert!(context
        .artifact_view()
        .capture_graph_selection()
        .native_groups
        .iter()
        .all(|group| group.binding.artifact() != original || group.original_ordinal == 7));
    let transport = NativeInputSelection::issue_checked(
        context.compiler_input_projection(),
        &context.compiler_metadata_snapshot().unwrap(),
        context.artifact_view(),
        offers,
    )
    .unwrap();
    let availability = &transport.availability.0[&original];
    assert!(availability.permits(7));
    assert!(!availability.permits(11));
    assert!(
        matches!(&availability.mode, NativeTransportMode::IssuedGroups)
            && availability.issued_ordinals() == BTreeSet::from([7])
    );
}

fn published_composition_histories(sparse: bool, interface_adapter_only: bool) {
    use crate::artifact_inventory::{
        ArtifactBindingNode, ArtifactInventoryFailure, IssuedNativeOffer,
    };
    let producer = [2; 32];
    let child_interface = crate::certified_products::fixture_module_interface(
        producer,
        "fixture",
        "Child",
        BTreeMap::new(),
    );
    let child_owner = identity("fixture", "Child");
    let root_owner = identity("fixture", "Root");
    let target = Arc::new(ArtifactEntry::canonical(
        crate::certified_products::fixture_module_interface(
            producer,
            "fixture",
            "Target",
            BTreeMap::new(),
        ),
    ));
    let ordinals = if sparse { [7, 11] } else { [3, 29] };
    let root = |version| {
        let product = crate::certified_products::fixture_finalized_product_with_requirements(
            crate::certified_products::tests::original_groups_fixture_with_interface(
                "Root",
                ordinals
                    .into_iter()
                    .map(|ordinal| (ordinal, vec![]))
                    .collect(),
                version,
                &BTreeMap::new(),
                b"published root interface".to_vec(),
            ),
            producer,
            Some(BTreeMap::from([(
                ("fixture".into(), "Child".into()),
                child_interface.interface_sha256(),
            )])),
        );
        let product = crate::certified_products::tests::recovered_witness_fixtures(&[product])
            .remove(0)
            .product;
        Arc::new(ArtifactEntry::original(producer, product).unwrap())
    };
    let native = root(1);
    let lexical = vec![
        ExactLexicalNode {
            owner: target.descriptor.owner.clone(),
            imports: vec![root_owner.clone()],
        },
        ExactLexicalNode {
            owner: root_owner.clone(),
            imports: vec![child_owner.clone()],
        },
        ExactLexicalNode {
            owner: child_owner.clone(),
            imports: vec![],
        },
    ];
    let mut failures = Vec::new();
    let mut completed = 0;
    for changed_child in [false, true] {
        let children = [
            Arc::new(ArtifactEntry::canonical(child_interface.clone())),
            Arc::new(ArtifactEntry::canonical(if changed_child {
                crate::certified_products::fixture_module_core(
                    &child_interface,
                    b"next Child Core".to_vec(),
                )
            } else {
                child_interface.clone()
            })),
        ];
        assert_eq!(
            children[0].descriptor.interface_sha256,
            children[1].descriptor.interface_sha256
        );
        assert_eq!(
            children[0].descriptor.id != children[1].descriptor.id,
            changed_child
        );
        for reverse in [false, true] {
            let views = children
                .iter()
                .enumerate()
                .map(|(index, child)| {
                    let inventory = ArtifactInventory::default();
                    let sibling = Arc::new(ArtifactEntry::canonical(
                        crate::certified_products::fixture_module_interface(
                            producer,
                            "fixture",
                            ["Left", "Right"][index],
                            BTreeMap::new(),
                        ),
                    ));
                    let mut entries = vec![native.clone(), child.clone(), target.clone(), sibling];
                    if reverse {
                        entries.reverse();
                    }
                    if sparse {
                        inventory
                            .admit_recovery_selection(
                                &inventory.empty_view(),
                                entries,
                                &BTreeSet::from([NativeGroupKey {
                                    artifact: native.descriptor.id,
                                    original_ordinal: 7,
                                }]),
                            )
                            .unwrap()
                    } else {
                        inventory
                            .admit_shared(&inventory.empty_view(), entries)
                            .unwrap()
                    }
                })
                .collect::<Vec<_>>();
            let binding = |index: usize| {
                *views[index]
                    .capture_graph_selection()
                    .namespace
                    .iter()
                    .find(|binding| binding.artifact == native.descriptor.id)
                    .unwrap()
            };
            assert_ne!(
                binding(0),
                binding(1),
                "equal native payloads retain distinct issuers"
            );
            let edges = views
                .iter()
                .flat_map(|view| view.binding_dependencies())
                .filter(|(from, to, _)| {
                    matches!(
                        (from, to),
                        (
                            ArtifactBindingNode::Artifact(_),
                            ArtifactBindingNode::Artifact(_)
                        )
                    )
                })
                .collect::<BTreeSet<_>>();
            for selected in 0..2 {
                let projection = CompilerInputProjection::from_issued_entries(&[
                    native.clone(),
                    children[selected].clone(),
                    target.clone(),
                ])
                .unwrap();
                let old =
                    IssuedNativeOffer::issue(native.descriptor.id, &views[1 - selected], None)
                        .unwrap();
                let support = crate::declaration_context::recover_fixture_compiler_support(
                    projection.clone(),
                    views[selected].clone(),
                    [old.clone(), old],
                )
                .unwrap();
                let issued = ExactDeclarationContext::from_authenticated_execution(
                    producer,
                    &views[selected],
                    projection,
                    lexical.clone(),
                    target.descriptor.owner.clone(),
                    &[
                        target.descriptor.owner.clone(),
                        root_owner.clone(),
                        child_owner.clone(),
                    ],
                )
                .unwrap()
                .extend_issued_compiler_support(&support)
                .unwrap();
                if interface_adapter_only {
                    check_interface_adapter(
                        &issued,
                        &views[selected],
                        native.descriptor.id,
                        children[selected].descriptor.id,
                    );
                    completed += 1;
                    continue;
                }
                let publication = issued
                    .issue_published_source_original("revision", "input", &root_owner)
                    .expect("the completed target directly imported the native root");
                if sparse {
                    assert_sparse_partial_transport(publication.context(), native.descriptor.id);
                }
                let publication_sha = publication.context().semantic_sha256();
                let publication_graph = publication
                    .context()
                    .artifact_view()
                    .capture_graph_selection();
                let published_role = publication
                    .context()
                    .compiler_input_projection()
                    .snapshot_roles()[&root_owner]
                    .clone();
                assert!(published_role.is_published_source_original());
                if !sparse && !changed_child && !reverse && selected == 0 {
                    use fvm_ipld_blockstore::Blockstore;
                    let foreign = views[1].capture_graph_selection();
                    let mut substitutions = Vec::new();
                    for (name, artifact) in [
                        ("carrier", native.descriptor.id),
                        ("child", children[selected].descriptor.id),
                    ] {
                        let replacement = *foreign
                            .namespace
                            .iter()
                            .find(|binding| binding.artifact == artifact)
                            .unwrap();
                        let mut graph = publication_graph.clone();
                        let current = graph
                            .namespace
                            .iter_mut()
                            .find(|binding| binding.artifact == artifact)
                            .unwrap();
                        assert_ne!(*current, replacement);
                        *current = replacement;
                        graph.namespace.sort();
                        substitutions.push((name, graph));
                    }
                    let mut graph = publication_graph.clone();
                    let parent = graph
                        .native_groups
                        .iter_mut()
                        .find(|group| group.binding.artifact() == native.descriptor.id)
                        .unwrap();
                    let previous = *parent;
                    let replacement = *foreign
                        .native_groups
                        .iter()
                        .find(|group| {
                            group.binding.artifact() == native.descriptor.id
                                && group.original_ordinal == previous.original_ordinal
                        })
                        .unwrap();
                    assert_ne!(previous, replacement);
                    *parent = replacement;
                    graph.native_groups.sort();
                    for node in graph.roots.iter_mut().chain(&mut graph.custody_roots) {
                        if *node == ArtifactBindingNode::Group(previous) {
                            *node = ArtifactBindingNode::Group(replacement);
                        }
                    }
                    graph.roots.sort();
                    graph.custody_roots.sort();
                    substitutions.push(("parent", graph));
                    let (_scratch, inventory, _) = super::history_properties::restored_graph_roles(
                        publication.context(),
                        &publication.context().compiler_input_roles(),
                    )
                    .unwrap();
                    for (name, graph) in substitutions {
                        let mut role = published_role.clone();
                        let CompilerInputRole::PublishedSourceOriginal { selection, .. } =
                            &mut role
                        else {
                            unreachable!()
                        };
                        *selection = crate::artifact_inventory::ExactArtifactSelection::from_recovered_graph_selection(graph);
                        let mut snapshot = publication.context().snapshot();
                        snapshot.native_fact_owner = None;
                        snapshot
                            .roles
                            .insert(root_owner.clone(), role.recorded_fact());
                        let store = fvm_ipld_blockstore::MemoryBlockstore::default();
                        let mut writer = crate::exact_context::Materializer::new(&store);
                        let written = writer.materialize_context(None, &snapshot).unwrap();
                        for (cid, bytes) in writer.finish().unwrap().into_blocks() {
                            store.put_keyed(&cid, &bytes).unwrap();
                        }
                        let decoded =
                            crate::exact_context::hydrate_context(&store, written.root()).unwrap();
                        assert!(
                            inventory
                                .context_for_expected_snapshot(written.root(), &decoded)
                                .is_err(),
                            "cold publication recovery must refuse a substituted {name} issuer"
                        );
                    }
                }
                assert_eq!(
                    publication
                        .context()
                        .compiler_input_projection()
                        .snapshot_roles()[&child_owner]
                        .interface(),
                    children[selected].descriptor.id
                );
                assert!(
                    edges.iter().all(|edge| publication
                        .context()
                        .artifact_view()
                        .binding_dependencies()
                        .contains(edge)),
                    "the publication retains both exact historical child graphs"
                );
                let expected_groups = views[selected].selected_native_groups();
                for start_empty in [false, true] {
                    let mut receiver = if start_empty {
                        ExactDeclarationContext::new(&[], &[], vec![]).unwrap()
                    } else {
                        issued.clone()
                    };
                    let initial = receiver.semantic_sha256();
                    let mut accepted = true;
                    for repetition in 0..3 {
                        match receiver
                            .clone()
                            .with_published_source_originals(&publication)
                        {
                            Ok(next) => {
                                if sparse {
                                    assert_sparse_partial_transport(&next, native.descriptor.id);
                                }
                                assert_eq!(
                                    next.compiler_input_projection().snapshot_roles()[&root_owner],
                                    published_role
                                );
                                assert_eq!(
                                    next.compiler_input_projection().snapshot_roles()[&child_owner]
                                        .interface(),
                                    children[selected].descriptor.id
                                );
                                assert!(next
                                    .compiler_input_projection()
                                    .snapshot_roles()
                                    .keys()
                                    .all(|owner| owner == &root_owner
                                        || owner == &child_owner
                                        || owner == &target.descriptor.owner));
                                assert_eq!(
                                    next.artifact_view().selected_native_groups(),
                                    expected_groups
                                );
                                assert!(edges.iter().all(|edge| {
                                    next.artifact_view().binding_dependencies().contains(edge)
                                }));
                                if repetition > 0 {
                                    assert_eq!(
                                        next.semantic_sha256(),
                                        receiver.semantic_sha256(),
                                        "reimport is idempotent"
                                    );
                                }
                                receiver = next;
                            }
                            Err(error) => {
                                failures.push(format!("changed_child={changed_child} reverse={reverse} selected={selected} start_empty={start_empty} repetition={repetition}: published consumer: {error}"));
                                if repetition == 0 {
                                    assert_eq!(receiver.semantic_sha256(), initial);
                                }
                                accepted = false;
                                break;
                            }
                        }
                    }
                    completed += usize::from(accepted);
                }
                assert_eq!(publication.context().semantic_sha256(), publication_sha);
                assert_eq!(
                    publication
                        .context()
                        .artifact_view()
                        .capture_graph_selection(),
                    publication_graph
                );
                if changed_child {
                    let competing_projection = CompilerInputProjection::from_issued_entries(&[
                        native.clone(),
                        children[1 - selected].clone(),
                        target.clone(),
                    ])
                    .unwrap();
                    let competing = ExactDeclarationContext::from_authenticated_execution(
                        producer,
                        &views[1 - selected],
                        competing_projection,
                        lexical.clone(),
                        target.descriptor.owner.clone(),
                        &[],
                    )
                    .unwrap();
                    assert!(
                        matches!(competing.with_published_source_originals(&publication),
                        Err(CompileError::ArtifactInventory(error))
                        if matches!(error.failure, ArtifactInventoryFailure::OwnerConflict { .. })),
                        "different selected canonical roles must refuse"
                    );
                }
                let alternative = issued
                    .issue_published_source_original("other revision", "input", &root_owner)
                    .unwrap();
                assert!(
                    publication
                        .context()
                        .as_ref()
                        .clone()
                        .with_published_source_originals(&alternative)
                        .is_err(),
                    "different publication policies must refuse"
                );
                let conflicting_root = root(2);
                assert_ne!(conflicting_root.descriptor.id, native.descriptor.id);
                let conflict_inventory = ArtifactInventory::default();
                let conflict_view = conflict_inventory
                    .admit_shared(
                        &conflict_inventory.empty_view(),
                        vec![
                            conflicting_root.clone(),
                            children[selected].clone(),
                            target.clone(),
                        ],
                    )
                    .unwrap();
                let conflict_projection = CompilerInputProjection::from_issued_entries(&[
                    conflicting_root,
                    children[selected].clone(),
                    target.clone(),
                ])
                .unwrap();
                let conflicting = ExactDeclarationContext::from_authenticated_execution(
                    producer,
                    &conflict_view,
                    conflict_projection,
                    lexical.clone(),
                    target.descriptor.owner.clone(),
                    &[],
                )
                .unwrap();
                assert!(
                    matches!(conflicting.with_published_source_originals(&publication),
                    Err(CompileError::ArtifactInventory(error))
                    if matches!(error.failure, ArtifactInventoryFailure::CompilerOriginalOfferConflict { .. })),
                    "different selected native originals must refuse"
                );
            }
        }
    }
    eprintln!(
        "published composition histories: completed={completed} failed={}\n{}",
        failures.len(),
        failures.join("\n")
    );
    assert!(
        failures.is_empty(),
        "published composition rejected an issuer-preserving history"
    );
    assert_eq!(completed, if interface_adapter_only { 8 } else { 16 });
}

#[test]
fn authored_certifier_keeps_published_subset_policy_separate_from_complete_demand() {
    let producer = [2; 32];
    let product = crate::certified_products::tests::recovered_witness_fixtures(&[
        crate::certified_products::fixture_finalized_product(
            crate::certified_products::tests::original_groups_fixture_with_interface(
                "PublishedSubset",
                vec![(3, vec![]), (29, vec![])],
                1,
                &BTreeMap::new(),
                b"published interface".to_vec(),
            ),
            producer,
        ),
    ])
    .remove(0)
    .product;
    let root = Arc::new(ArtifactEntry::original(producer, product).unwrap());
    let target = Arc::new(ArtifactEntry::canonical(
        crate::certified_products::fixture_module_interface(
            producer,
            "fixture",
            "Target",
            BTreeMap::new(),
        ),
    ));
    let inventory = ArtifactInventory::default();
    let early = NativeGroupKey {
        artifact: root.descriptor.id,
        original_ordinal: 3,
    };
    let view = inventory
        .admit_recovery_selection(
            &inventory.empty_view(),
            vec![root.clone(), target.clone()],
            &BTreeSet::from([early]),
        )
        .unwrap();
    let projection =
        CompilerInputProjection::from_issued_entries(&[root.clone(), target.clone()]).unwrap();
    let issued = ExactDeclarationContext::from_authenticated_execution(
        producer,
        &view,
        projection.clone(),
        vec![
            ExactLexicalNode {
                owner: target.descriptor.owner.clone(),
                imports: vec![root.descriptor.owner.clone()],
            },
            ExactLexicalNode {
                owner: root.descriptor.owner.clone(),
                imports: vec![],
            },
        ],
        target.descriptor.owner.clone(),
        &[
            target.descriptor.owner.clone(),
            root.descriptor.owner.clone(),
        ],
    )
    .unwrap();
    let publication = issued
        .issue_published_source_original("revision", "input", &root.descriptor.owner)
        .unwrap();
    let published = publication.context();
    let before = published.artifact_view().capture_graph_selection();
    let before_sha = published.semantic_sha256();
    let complete_inventory = ArtifactInventory::default();
    let complete = complete_inventory
        .admit_shared(&complete_inventory.empty_view(), vec![root.clone(), target])
        .unwrap();
    let selection = crate::certified_products::CertifiedSourceSelection::from_compiler_projection(
        &projection,
        &complete.metadata_snapshot(),
        &tidepool_repr::execution_schema::InventoryOperation::new(Default::default()),
    )
    .unwrap();
    let originals = selection.selected_original_closure(&complete).unwrap();
    let original_graph = originals.native_closure().capture_graph_selection();
    let role =
        published.compiler_input_projection().snapshot_roles()[&root.descriptor.owner].clone();
    let policy =
        crate::artifact_inventory::NativeOriginalRequirement::from_role(role.clone()).unwrap();
    assert_eq!(
        policy
            .selection
            .graph_selection()
            .roots
            .iter()
            .filter(|node| matches!(
                node,
                crate::artifact_inventory::ArtifactBindingNode::Group(_)
            ))
            .count(),
        1
    );
    assert_eq!(originals.native_closure().selected_native_groups().len(), 2);
    let legacy_incoming = published
        .compiler_input_projection()
        .within_view(originals.native_closure());
    let (policy_only, _) = originals
        .native_closure()
        .merge_fixture_compiler_inputs(
            &legacy_incoming,
            published.artifact_view(),
            published.compiler_input_projection(),
        )
        .unwrap();
    assert_eq!(
        policy_only.selected_native_groups(),
        BTreeSet::from([early]),
        "role-policy merge alone loses the complete closure's second group"
    );
    let support = certified_artifact_view(&originals, published).unwrap();
    let demanded = (**published)
        .clone()
        .rebuild(|facts| facts.compiler_support = support.clone());
    let extended = demanded
        .clone()
        .extend_interface_context(&demanded)
        .unwrap();
    assert_eq!(
        extended.artifact_view().capture_graph_selection().roots,
        demanded.artifact_view().capture_graph_selection().roots,
        "type evidence must preserve the existing complete demand independently of published subset policy"
    );

    assert_eq!(
        support.projection().snapshot_roles()[&root.descriptor.owner],
        role
    );
    for group in original_graph.roots.iter().filter(|node| {
        matches!(
            node,
            crate::artifact_inventory::ArtifactBindingNode::Group(_)
        )
    }) {
        assert!(
            support
                .artifacts()
                .capture_graph_selection()
                .roots
                .contains(group),
            "published policy replaced the complete issued demand"
        );
    }
    assert!(originals
        .native_closure()
        .binding_dependencies()
        .iter()
        .all(|edge| support.artifacts().binding_dependencies().contains(edge)));
    assert_eq!(published.artifact_view().capture_graph_selection(), before);
    assert_eq!(
        published.semantic_sha256(),
        before_sha,
        "joining custody must not mutate the immutable publication"
    );
    for shared_type in [false, true] {
        for fresh_group_count in [1, 2, 3] {
            let fresh = crate::certified_products::tests::recovered_witness_fixtures(&[
                crate::certified_products::fixture_finalized_product_with_requirements(
                    crate::certified_products::tests::original_groups_fixture_with_interface(
                        "UnrelatedAuthored",
                        (0..fresh_group_count)
                            .map(|index| (7 + index * 10, vec![]))
                            .collect(),
                        2 + fresh_group_count as u8,
                        &BTreeMap::new(),
                        b"fresh authored interface".to_vec(),
                    ),
                    producer,
                    Some(if shared_type {
                        BTreeMap::from([(
                            (
                                root.descriptor.owner.unit.clone(),
                                root.descriptor.owner.module.clone(),
                            ),
                            root.descriptor.interface_sha256,
                        )])
                    } else {
                        BTreeMap::new()
                    }),
                ),
            ])
            .remove(0)
            .product;
            let fresh_entry = Arc::new(ArtifactEntry::original(producer, fresh).unwrap());
            let ArtifactPayload::Original(root_product) = &root.payload else {
                unreachable!()
            };
            let root_interface = Arc::new(ArtifactEntry::canonical(
                root_product.module_interface().unwrap().clone(),
            ));
            let fresh_inventory = ArtifactInventory::default();
            let fresh_view = fresh_inventory
                .admit_shared(
                    &fresh_inventory.empty_view(),
                    vec![fresh_entry.clone(), root_interface.clone()],
                )
                .unwrap();
            let fresh_projection =
                CompilerInputProjection::from_issued_entries(&[fresh_entry, root_interface])
                    .unwrap();
            let fresh_selection =
                crate::certified_products::CertifiedSourceSelection::from_compiler_projection(
                    &fresh_projection,
                    &fresh_view.metadata_snapshot(),
                    &tidepool_repr::execution_schema::InventoryOperation::new(Default::default()),
                )
                .unwrap();
            let fresh_originals = fresh_selection
                .selected_original_closure(&fresh_view)
                .unwrap();
            let baseline = published
                .as_ref()
                .clone()
                .extend_issued_compiler_support(
                    &crate::declaration_context::issue_fixture_compiler_support(
                        fresh_projection,
                        fresh_view,
                    )
                    .unwrap(),
                )
                .unwrap();
            assert!(
                baseline
                    .compiler_input_projection()
                    .within_view(fresh_originals.native_closure())
                    .validate(fresh_originals.native_closure())
                    .is_err(),
                "within_view must not silently erase missing published policy"
            );
            let certified = certified_artifact_view(&fresh_originals, &baseline).unwrap();
            assert_eq!(
                certified.artifacts().selected_native_groups().len(),
                fresh_group_count as usize + 1
            );
            for selected in fresh_originals
                .native_closure()
                .capture_graph_selection()
                .roots
                .iter()
                .filter(|node| {
                    matches!(
                        node,
                        crate::artifact_inventory::ArtifactBindingNode::Group(_)
                    )
                })
            {
                assert!(certified
                    .artifacts()
                    .capture_graph_selection()
                    .roots
                    .contains(selected));
            }
            assert_eq!(
                certified.projection().snapshot_roles()[&root.descriptor.owner],
                role
            );
            assert!(certified
                .artifacts()
                .capture_graph_selection()
                .roots
                .contains(&crate::artifact_inventory::ArtifactBindingNode::Group(
                    *policy
                        .selection
                        .graph_selection()
                        .native_groups
                        .first()
                        .unwrap()
                )));
        }
    }
}

#[test]
fn published_native_custody_does_not_require_dependency_namespace_originals() {
    fn receipts_by_artifact<T>(
        references: &[T],
        artifact_id: impl Fn(&T) -> ArtifactId,
    ) -> BTreeMap<ArtifactId, &T> {
        let indexed = references
            .iter()
            .map(|reference| (artifact_id(reference), reference))
            .collect::<BTreeMap<_, _>>();
        assert_eq!(
            indexed.len(),
            references.len(),
            "duplicate artifact receipt"
        );
        indexed
    }

    let producer = [2; 32];
    let native = |module: &str, groups, version, requirements| {
        Arc::new(
            ArtifactEntry::original(
                producer,
                crate::certified_products::tests::recovered_witness_fixtures(&[
                    crate::certified_products::fixture_finalized_product_with_requirements(
                        crate::certified_products::tests::original_groups_fixture_with_interface(
                            module,
                            groups,
                            version,
                            &BTreeMap::new(),
                            format!("interface-{module}").into_bytes(),
                        ),
                        producer,
                        Some(requirements),
                    ),
                ])
                .remove(0)
                .product,
            )
            .unwrap(),
        )
    };
    let source = |entry: &ArtifactEntry, ordinal, occurrence: &str| {
        let ArtifactPayload::Original(product) = &entry.payload else {
            unreachable!()
        };
        PendingImportOwner::Source {
            owner: product.owner().clone(),
            original_ordinal: ordinal,
            binder: SymbolIdentity {
                unit: product.owner().unit.clone(),
                module: product.owner().module.clone(),
                namespace: "value".into(),
                occurrence: occurrence.into(),
                record_parent: None,
            },
        }
    };
    let leaf = native("Leaf", vec![(7, vec![])], 1, BTreeMap::new());
    let helper_requirements = BTreeMap::from([(
        ("fixture".into(), "Leaf".into()),
        leaf.descriptor.interface_sha256,
    )]);
    let old = native(
        "Helper",
        vec![(7, vec![source(&leaf, 7, "entry")])],
        1,
        helper_requirements.clone(),
    );
    let current = native(
        "Helper",
        vec![(7, vec![source(&leaf, 7, "entry")])],
        2,
        helper_requirements.clone(),
    );
    let future = native(
        "Helper",
        vec![(7, vec![source(&leaf, 7, "entry")])],
        3,
        helper_requirements,
    );
    let root = native(
        "Root",
        vec![(3, vec![]), (29, vec![source(&old, 7, "entry")])],
        1,
        BTreeMap::from([(
            ("fixture".into(), "Helper".into()),
            old.descriptor.interface_sha256,
        )]),
    );
    let target = Arc::new(ArtifactEntry::canonical(
        crate::certified_products::fixture_module_interface(
            producer,
            "fixture",
            "Target",
            BTreeMap::new(),
        ),
    ));
    let ArtifactPayload::Original(old_product) = &old.payload else {
        unreachable!()
    };
    let helper_interface = Arc::new(ArtifactEntry::canonical(
        old_product.module_interface().unwrap().clone(),
    ));
    let ArtifactPayload::Original(leaf_product) = &leaf.payload else {
        unreachable!()
    };
    let leaf_interface = Arc::new(ArtifactEntry::canonical(
        leaf_product.module_interface().unwrap().clone(),
    ));
    let early = NativeGroupKey {
        artifact: root.descriptor.id,
        original_ordinal: 3,
    };
    for current_original_role in [false, true] {
        let inventory = ArtifactInventory::default();
        let view = inventory
            .admit_recovery_selection(
                &inventory.empty_view(),
                vec![
                    root.clone(),
                    old.clone(),
                    current.clone(),
                    leaf.clone(),
                    target.clone(),
                ],
                &BTreeSet::from([early]),
            )
            .unwrap();
        let projection = CompilerInputProjection::from_issued_entries(&[
            root.clone(),
            if current_original_role {
                current.clone()
            } else {
                helper_interface.clone()
            },
            leaf_interface.clone(),
            target.clone(),
        ])
        .unwrap();
        let issued = ExactDeclarationContext::from_authenticated_execution(
            producer,
            &view,
            projection,
            vec![
                ExactLexicalNode {
                    owner: target.descriptor.owner.clone(),
                    imports: vec![root.descriptor.owner.clone()],
                },
                ExactLexicalNode {
                    owner: root.descriptor.owner.clone(),
                    imports: vec![old.descriptor.owner.clone()],
                },
                ExactLexicalNode {
                    owner: old.descriptor.owner.clone(),
                    imports: vec![leaf.descriptor.owner.clone()],
                },
                ExactLexicalNode {
                    owner: leaf.descriptor.owner.clone(),
                    imports: vec![],
                },
            ],
            target.descriptor.owner.clone(),
            &[
                target.descriptor.owner.clone(),
                root.descriptor.owner.clone(),
                old.descriptor.owner.clone(),
                leaf.descriptor.owner.clone(),
            ],
        )
        .unwrap();
        let publication = issued
            .issue_published_source_original("revision", "input", &root.descriptor.owner)
            .unwrap();
        let published = publication.context();
        let published_role =
            published.compiler_input_projection().snapshot_roles()[&root.descriptor.owner].clone();
        let birth = published
            .compiler_source_support()
            .publication_birth_support(&root.descriptor.owner)
            .unwrap();
        let assert_birth = |context: &ExactDeclarationContext| {
            assert_eq!(
                &context.compiler_input_projection().snapshot_roles()[&root.descriptor.owner],
                &published_role
            );
            let reopened_birth = context
                .compiler_source_support()
                .publication_birth_support(&root.descriptor.owner)
                .unwrap();
            assert_eq!(
                reopened_birth.artifacts().capture_graph_selection(),
                birth.artifacts().capture_graph_selection()
            );
            assert_eq!(
                reopened_birth.native_availability().persistent_facts(),
                birth.native_availability().persistent_facts()
            );
        };
        assert!(published
            .artifact_view()
            .artifact_ids()
            .contains(&old.descriptor.id));
        assert!(published
            .artifact_view()
            .artifact_ids()
            .contains(&leaf.descriptor.id));
        assert_eq!(
            published
                .artifact_view()
                .artifact_ids()
                .contains(&current.descriptor.id),
            current_original_role
        );
        assert_eq!(
            published.artifact_view().selected_native_groups(),
            BTreeSet::from([early])
        );
        let helper_selected =
            &published.compiler_metadata_snapshot().unwrap().entries[&old.descriptor.owner];
        if current_original_role {
            assert_eq!(helper_selected.descriptor.id, current.descriptor.id);
        } else {
            assert!(matches!(
                helper_selected.payload,
                ArtifactPayload::Canonical(_)
            ));
        }
        assert!(matches!(
            published.compiler_metadata_snapshot().unwrap().entries[&leaf.descriptor.owner].payload,
            ArtifactPayload::Canonical(_)
        ));
        let composed = ExactDeclarationContext::new(&[], &[], vec![])
            .unwrap()
            .with_published_source_originals(&publication)
            .unwrap();
        assert_eq!(composed.semantic_sha256(), published.semantic_sha256());
        let reopened = published.published_source_original_selections().unwrap();
        assert_eq!(
            reopened[0].context().semantic_sha256(),
            published.semantic_sha256()
        );
        let admitted = published
            .artifact_view()
            .inventory()
            .admit_recovery_selection(
                published.artifact_view(),
                vec![root.clone()],
                &BTreeSet::from([
                    NativeGroupKey {
                        artifact: root.descriptor.id,
                        original_ordinal: 29,
                    },
                    NativeGroupKey {
                        artifact: old.descriptor.id,
                        original_ordinal: 7,
                    },
                    NativeGroupKey {
                        artifact: leaf.descriptor.id,
                        original_ordinal: 7,
                    },
                ]),
            )
            .unwrap();
        assert_eq!(
            admitted.selected_native_groups(),
            BTreeSet::from([
                early,
                NativeGroupKey {
                    artifact: root.descriptor.id,
                    original_ordinal: 29
                },
                NativeGroupKey {
                    artifact: old.descriptor.id,
                    original_ordinal: 7
                },
                NativeGroupKey {
                    artifact: leaf.descriptor.id,
                    original_ordinal: 7
                },
            ])
        );
        // Recovery checks this exact graph's custody and recorded source
        // decisions without constructing an executable target.
        let reopen_view = |view: &ArtifactView| -> Result<ExactDeclarationContext, CompileError> {
            let support = recover_fixture_compiler_support(
                published.compiler_input_projection().clone(),
                view.clone(),
                published
                    .compiler_source_support()
                    .native_offers()
                    .iter()
                    .cloned(),
            )?
            .restore_snapshot_evidence(
                &published.snapshot(),
                &BTreeMap::new(),
                &mut crate::recovery_artifacts::PackageInterfaceValidation::default(),
            )?;
            published
                .as_ref()
                .clone()
                .rebuild(|facts| facts.compiler_support = support)
                .finish()
        };
        let continued = reopen_view(&admitted).unwrap();
        assert_eq!(
            continued.artifact_view().selected_native_groups(),
            admitted.selected_native_groups()
        );
        assert_eq!(
            continued
                .artifact_view()
                .capture_graph_selection()
                .native_groups,
            admitted.capture_graph_selection().native_groups
        );
        let reopened_after_demand = continued.published_source_original_selections().unwrap();
        assert_birth(reopened_after_demand[0].context());
        assert_eq!(
            reopened_after_demand[0]
                .context()
                .artifact_view()
                .capture_graph_selection()
                .native_groups,
            admitted.capture_graph_selection().native_groups
        );
        let ambient = admitted
            .inventory()
            .admit_shared_with_demand(
                &admitted,
                vec![future.clone()],
                NativeArtifactDemand::ScopeInterfaces,
            )
            .unwrap();
        let with_ambient = reopen_view(&ambient).unwrap();
        assert_eq!(
            with_ambient.artifact_view().selected_native_groups(),
            ambient.selected_native_groups()
        );
        let frozen = with_ambient.published_source_original_selections().unwrap();
        assert_birth(frozen[0].context());
        assert_eq!(
            frozen[0]
                .context()
                .artifact_view()
                .capture_graph_selection()
                .native_groups,
            admitted.capture_graph_selection().native_groups
        );
        assert!(!frozen[0]
            .context()
            .artifact_view()
            .artifact_ids()
            .contains(&future.descriptor.id));
        let canonical_binding = *published
            .artifact_view()
            .capture_graph_selection()
            .namespace
            .iter()
            .find(|binding| binding.artifact == helper_interface.descriptor.id)
            .unwrap();
        // The competing original earns its own ordinary checked owner using
        // the same issued type support, not merely equal interface bytes.
        let types = published
            .compiler_source_support()
            .selected_interfaces()
            .unwrap()
            .support_for_owners(vec![old.descriptor.owner.clone()])
            .unwrap();
        assert!(types.artifacts().selected_native_groups().is_empty());
        let future_view = types
            .artifacts()
            .inventory()
            .admit_shared(types.artifacts(), vec![future.clone(), leaf.clone()])
            .unwrap();
        let future_context = ExactDeclarationContext::from_authenticated_execution(
            producer,
            &future_view,
            CompilerInputProjection::from_issued_entries(&[future.clone(), leaf_interface.clone()])
                .unwrap(),
            vec![
                ExactLexicalNode {
                    owner: future.descriptor.owner.clone(),
                    imports: vec![leaf.descriptor.owner.clone()],
                },
                ExactLexicalNode {
                    owner: leaf.descriptor.owner.clone(),
                    imports: vec![],
                },
            ],
            future.descriptor.owner.clone(),
            &[
                future.descriptor.owner.clone(),
                leaf.descriptor.owner.clone(),
            ],
        )
        .expect("ordinary Future issuer must complete before composition");
        assert!(future_context
            .compiler_input_roles()
            .iter()
            .any(|role| role.original() == Some(future.descriptor.id)));
        let future_offer = future_context
            .compiler_source_support()
            .native_offers()
            .iter()
            .find(|offer| offer.original() == future.descriptor.id && offer.is_full())
            .expect("the ordinary Future issuer supplies its own checked Full offer")
            .clone();
        assert!(
            future_context
                .artifact_view()
                .capture_graph_selection()
                .namespace
                .contains(&canonical_binding),
            "the Future issuer must share the publication's exact canonical binding"
        );
        assert_ne!(future.descriptor.id, current.descriptor.id);
        let replacement = future_context.with_published_source_originals(&publication);
        if current_original_role {
            let Err(CompileError::ArtifactInventory(error)) = replacement else {
                panic!("competing selected originals must refuse during actual composition")
            };
            assert!(matches!(error.failure,
                crate::artifact_inventory::ArtifactInventoryFailure::CompilerOriginalOfferConflict {
                    owner, existing, incoming,
                } if owner == current.descriptor.owner
                    && existing == future.descriptor.id && incoming == current.descriptor.id));
        } else {
            let replacement = replacement.unwrap();
            assert!(replacement
                .compiler_input_roles()
                .iter()
                .any(|role| role.original() == Some(future.descriptor.id)));
            let reopened = replacement.published_source_original_selections().unwrap();
            let frozen = reopened[0].context();
            assert_birth(frozen);
            assert_eq!(
                frozen
                    .artifact_view()
                    .capture_graph_selection()
                    .native_groups,
                replacement
                    .artifact_view()
                    .capture_graph_selection()
                    .native_groups
            );
            assert!(frozen
                .artifact_view()
                .capture_graph_selection()
                .namespace
                .contains(&canonical_binding));
            assert!(frozen
                .artifact_view()
                .contains_namespace_artifact(future.descriptor.id));
            assert!(frozen
                .compiler_input_roles()
                .iter()
                .any(|role| role.original() == Some(future.descriptor.id)));
            let CompilerInputRole::PublishedSourceOriginal { selection, .. } = &published_role
            else {
                unreachable!()
            };
            assert!(!selection
                .graph_selection()
                .bindings
                .iter()
                .any(|binding| binding.artifact == future.descriptor.id));
            let frozen_graph = frozen
                .compiler_source_support()
                .publication_birth_support(&root.descriptor.owner)
                .unwrap()
                .artifacts()
                .capture_graph_selection();
            assert_eq!(frozen_graph.roots, selection.graph_selection().roots);
            assert_eq!(
                frozen_graph.namespace,
                selection.graph_selection().namespace
            );
            for edge in published.artifact_view().binding_dependencies() {
                assert!(frozen
                    .artifact_view()
                    .binding_dependencies()
                    .contains(&edge));
            }
            if frozen
                .artifact_view()
                .artifact_ids()
                .contains(&future.descriptor.id)
            {
                assert!(frozen
                    .compiler_source_support()
                    .native_offers()
                    .iter()
                    .any(|offer| offer.same_issuance(&future_offer)));
            }
            // Current Future selection does not alter the publication's
            // retained exact native graph.
        }
        let recovery_scratch = tempfile::tempdir().unwrap();
        let (published_products, published_interfaces) = partition_native_recovery_references(
            published
                .materialize_recovery_artifacts(
                    recovery_scratch.path(),
                    &published
                        .artifact_view()
                        .artifact_ids()
                        .into_iter()
                        .collect(),
                )
                .unwrap(),
        );
        let selection: crate::artifact_inventory::ArtifactGraphSelection = serde_json::from_slice(
            &serde_json::to_vec(&published.artifact_view().capture_graph_selection()).unwrap(),
        )
        .unwrap();
        let edges: Vec<_> = serde_json::from_slice(
            &serde_json::to_vec(&published.artifact_view().binding_dependencies()).unwrap(),
        )
        .unwrap();
        let recovery_inventory = RecoveredArtifactInventory::capture_bound(
            recovery_scratch.path(),
            &published_products,
            &published_interfaces,
            &[],
            &[],
            &published.artifact_view().descriptors(),
            &selection.bindings,
            &edges,
        )
        .unwrap();
        assert!(
            recovery_inventory
                .context_with_roles(
                    &published.artifact_view().artifact_ids(),
                    &published
                        .artifact_view()
                        .selected_native_groups()
                        .into_iter()
                        .collect::<Vec<_>>(),
                    &published.compiler_input_roles(),
                    published.owner.facts.lexical.to_vec(),
                )
                .is_err(),
            "flat recovery cannot recreate an issuer's exact bound selection"
        );
        let recovered = recovery_inventory
            .context_with_graph_selection(
                &selection,
                &published.compiler_input_roles(),
                published.owner.facts.lexical.to_vec(),
            )
            .unwrap();
        let support = recovered
            .compiler_source_support()
            .restore_snapshot_evidence(
                &published.snapshot(),
                &BTreeMap::new(),
                &mut crate::recovery_artifacts::PackageInterfaceValidation::default(),
            )
            .unwrap();
        let recovered = recovered
            .rebuild(|facts| facts.compiler_support = support)
            .finish()
            .unwrap();
        assert_eq!(recovered.semantic_sha256(), published.semantic_sha256());
        for snapshot in [published.as_ref(), &continued] {
            let scratch = tempfile::tempdir().unwrap();
            let products = recovery_artifacts::materialize_certified_products(
                scratch.path(),
                producer,
                &snapshot.recovery_products(),
            )
            .unwrap();
            let interfaces = snapshot
                .materialize_module_interfaces(scratch.path())
                .unwrap();
            let reused_root = tempfile::tempdir().unwrap();
            let (reused_products, reused_interfaces) = partition_native_recovery_references(
                snapshot
                    .materialize_recovery_artifacts(
                        reused_root.path(),
                        &snapshot
                            .artifact_view()
                            .artifact_ids()
                            .into_iter()
                            .collect(),
                    )
                    .unwrap(),
            );
            let product_id = |reference: &RecoveryArtifactRef| {
                crate::artifact_inventory::ArtifactDescriptor::from_recovery_product(reference).id
            };
            let interface_id = |reference: &recovery_artifacts::RecoveryModuleInterfaceRef| {
                crate::artifact_inventory::ArtifactDescriptor::from_recovery_module_interface(
                    reference,
                )
                .id
            };
            assert_eq!(
                receipts_by_artifact(&reused_products, product_id),
                receipts_by_artifact(&products, product_id),
            );
            assert_eq!(
                receipts_by_artifact(&reused_interfaces, interface_id),
                receipts_by_artifact(&interfaces, interface_id),
            );
            for interface in &reused_interfaces {
                assert!(reused_root
                    .path()
                    .join(&interface.certificate_path)
                    .is_file());
                assert!(reused_root
                    .path()
                    .join(&interface.interface.interface_path)
                    .is_file());
            }
            // Complete owner evidence uses the strong recorded roles;
            // legacy graph DTOs can omit unused available proof scopes.
            let roles = snapshot.compiler_input_roles();
            let snapshot_selection: crate::artifact_inventory::ArtifactGraphSelection =
                serde_json::from_slice(
                    &serde_json::to_vec(&snapshot.artifact_view().capture_graph_selection())
                        .unwrap(),
                )
                .unwrap();
            let snapshot_edges: Vec<_> = serde_json::from_slice(
                &serde_json::to_vec(&snapshot.artifact_view().binding_dependencies()).unwrap(),
            )
            .unwrap();
            let durable = RecoveredArtifactInventory::capture_bound(
                scratch.path(),
                &products,
                &interfaces,
                &[],
                &[],
                &snapshot.artifact_view().descriptors(),
                &snapshot_selection.bindings,
                &snapshot_edges,
            )
            .unwrap()
            .context_with_graph_selection(
                &snapshot_selection,
                &roles,
                snapshot.owner.facts.lexical.to_vec(),
            )
            .unwrap();
            let support = durable
                .compiler_source_support()
                .restore_snapshot_evidence(
                    &snapshot.snapshot(),
                    &BTreeMap::new(),
                    &mut crate::recovery_artifacts::PackageInterfaceValidation::default(),
                )
                .unwrap();
            let durable = durable
                .rebuild(|facts| facts.compiler_support = support)
                .finish()
                .unwrap();
            assert_eq!(durable.artifact_view(), snapshot.artifact_view());
            assert_eq!(
                durable.compiler_input_roles(),
                snapshot.compiler_input_roles()
            );
            assert_eq!(durable.lexical_graph(), snapshot.lexical_graph());
            assert_eq!(
                durable.original_instance_environment(),
                &OriginalInstanceEnvironment::Unknown,
                "native recovery does not reconstruct a completed source-compilation proof"
            );
            let reopened = durable.published_source_original_selections().unwrap();
            assert_birth(reopened[0].context());
            assert_eq!(
                reopened[0]
                    .context()
                    .artifact_view()
                    .capture_graph_selection()
                    .native_groups,
                snapshot
                    .artifact_view()
                    .capture_graph_selection()
                    .native_groups
            );
        }
        let mut missing = recovery_inventory;
        missing.entries.remove(&old.descriptor.id);
        assert!(
            missing
                .context_with_graph_selection(
                    &selection,
                    &published.compiler_input_roles(),
                    published.owner.facts.lexical.to_vec(),
                )
                .is_err(),
            "recovery cannot replace missing exact child custody with its canonical interface"
        );
    }
}
